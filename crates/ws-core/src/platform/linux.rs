//! Linux 구현.
//!
//! 배포판마다 있거나 없거나 출력 형식이 다른 외부 명령(lsblk, blkid, fdisk 등)은 쓰지 않습니다.
//! 커널이 제공하는 `/sys/block`, `/proc/self/mountinfo`, `/proc` 와 udev 데이터베이스(`/run/udev/data`, 없으면 생략)만 읽습니다.
//!
//! - 디스크: sd*, nvme*n*, vd*, xvd*, hd*, mmcblk* (물리), md* (RAID), dm-* (LVM, LUKS, 멀티패스), loop* (파일 연결)
//! - 원본은 O_RDONLY | O_DIRECT로 엽니다 (페이지 캐시 = 대상 서버 메모리를 덮어쓰지 않음)
//! - LVM, LUKS 볼륨이 루트(/)를 담고 있으면 그 아래 물리 디스크까지 "시스템" 역할을 붙입니다

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::model::{DiskInfo, HostInfo, PartitionInfo, ProcInfo, VolumeInfo};
use crate::source::{drop_cache, open_device_readonly, FileSource, Source};
use crate::target::Target;

pub const ADMIN_HINT: super::Bilingual = super::Bilingual {
    ko: "root 권한이 필요합니다. sudo로 다시 실행하세요. 예: sudo ./WSCollector-CLI list",
    en: "Root privileges are required. Run again with sudo, e.g. sudo ./WSCollector-CLI list",
};

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read_trim(p)?.parse().ok()
}

// -- 호스트 ----------------------------------------------------------------

fn os_release() -> HashMap<String, String> {
    let text = fs::read_to_string("/etc/os-release").or_else(|_| fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()))
        .collect()
}

fn timezone_name() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() {
            return tz.to_string();
        }
    }
    if let Some(t) = read_trim("/etc/timezone").filter(|s| !s.is_empty()) {
        return t;
    }
    if let Ok(link) = fs::read_link("/etc/localtime") {
        let s = link.display().to_string();
        if let Some((_, name)) = s.split_once("zoneinfo/") {
            return name.to_string();
        }
    }
    String::new()
}

pub fn host_info() -> HostInfo {
    let osr = os_release();
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let cpu_field = |key: &str| {
        cpuinfo
            .lines()
            .find(|l| l.split(':').next().map(str::trim) == Some(key))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string())
    };
    let cores = cpuinfo.lines().filter(|l| l.starts_with("processor")).count();
    let cpu = cpu_field("model name").or_else(|| cpu_field("Model")).or_else(|| cpu_field("Hardware")).unwrap_or_else(|| std::env::consts::ARCH.to_string());
    let cpu = if cores > 0 { crate::tr!(format!("{cpu} ({cores}코어)"), format!("{cpu} ({cores} cores)")) } else { cpu };
    let mem_kb = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| m.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()))
        .unwrap_or(0);
    let uptime = read_trim("/proc/uptime").and_then(|s| s.split_whitespace().next().and_then(|x| x.parse::<f64>().ok())).unwrap_or(0.0);
    let now = chrono::Local::now();
    let boot = now - chrono::Duration::milliseconds((uptime * 1000.0) as i64);
    let tz = timezone_name();
    let model = read_trim("/sys/class/dmi/id/product_name")
        .or_else(|| read_trim("/proc/device-tree/model").map(|s| s.trim_end_matches('\0').to_string()))
        .unwrap_or_default();
    HostInfo {
        hostname: read_trim("/proc/sys/kernel/hostname").unwrap_or_default(),
        os_name: osr.get("PRETTY_NAME").or_else(|| osr.get("NAME")).cloned().unwrap_or_else(|| "Linux".into()),
        os_version: osr.get("VERSION_ID").cloned().unwrap_or_default(),
        os_build: read_trim("/proc/sys/kernel/osrelease").unwrap_or_default(),
        arch: std::env::consts::ARCH.into(),
        cpu,
        ram_bytes: mem_kb * 1024,
        manufacturer: read_trim("/sys/class/dmi/id/sys_vendor").unwrap_or_default(),
        model,
        boot_time: boot.format("%Y-%m-%d %H:%M:%S").to_string(),
        local_time: now.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        timezone: if tz.is_empty() { now.format("UTC%:z").to_string() } else { format!("{tz} (UTC{})", now.format("%:z")) },
    }
}

pub fn is_elevated() -> bool {
    // SAFETY: 인자 없는 시스템 호출
    unsafe { libc::geteuid() == 0 }
}

pub fn footprint_notes() -> Vec<String> {
    vec![
        crate::tr!(
            "Linux가 남기는 기록: 실행한 셸의 명령 기록(~/.bash_history 등, 셸 종료 시 저장), sudo 사용 기록(/var/log/auth.log, secure, journald)",
            "Artifacts left by Linux: shell command history (~/.bash_history etc., saved on shell exit), sudo usage records (/var/log/auth.log, secure, journald)"
        )
        .into(),
        crate::tr!(
            "Linux가 남기는 기록: USB 연결, 마운트 기록(dmesg, journald, syslog), auditd 실행 기록(설정된 경우)",
            "Artifacts left by Linux: USB connection and mount records (dmesg, journald, syslog), auditd execution records (if configured)"
        )
        .into(),
    ]
}

// -- 마운트 ----------------------------------------------------------------

#[derive(Debug, Clone)]
struct Mount {
    dev: (u32, u32),
    point: String,
    fstype: String,
    source: String,
    /// 파일시스템 옵션 (overlay의 lowerdir, upperdir 등)
    opts: String,
}

/// mountinfo의 8진수 이스케이프(\040 = 공백) 풀기
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_dev(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.trim().split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn mounts() -> Vec<Mount> {
    let text = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            let r: Vec<&str> = right.split(' ').collect();
            Some(Mount {
                dev: parse_dev(f.get(2)?)?,
                point: unescape(f.get(4)?),
                fstype: r.first()?.to_string(),
                source: unescape(r.get(1).unwrap_or(&"")),
                // 옵션은 이스케이프된 채로 둠: 경로 안 쉼표(\054)가 옵션 구분 쉼표로 바뀌지 않게, 나눈 뒤 값마다 풂
                opts: r.get(2).map(|o| o.to_string()).unwrap_or_default(),
            })
        })
        .collect()
}

/// 마운트의 실제 블록 장치 번호. btrfs 등은 가상 번호(major 0)라서 원본 장치 경로로 다시 찾음
fn mount_block_dev(m: &Mount) -> Option<(u32, u32)> {
    if m.dev.0 != 0 {
        return Some(m.dev);
    }
    if m.source.starts_with("/dev/") {
        let md = fs::metadata(&m.source).ok()?;
        let rdev = md.rdev();
        return Some((libc::major(rdev), libc::minor(rdev)));
    }
    None
}

/// overlay 옵션에서 아래 경로들: upperdir(쓰기가 실제로 가는 곳), workdir, lowerdir(여러 개는 ':'로 구분)
fn overlay_dirs(opts: &str) -> Vec<String> {
    let mut v = Vec::new();
    for o in opts.split(',') {
        if let Some((k, val)) = o.split_once('=') {
            let val = &unescape(val);
            match k {
                "upperdir" | "workdir" => v.push(val.to_string()),
                // lowerdir=/a:/b (\: 는 경로 안의 콜론)
                "lowerdir" => {
                    let mut cur = String::new();
                    let mut chars = val.chars().peekable();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' if chars.peek() == Some(&':') => cur.push(chars.next().unwrap()),
                            ':' => {
                                if !cur.is_empty() {
                                    v.push(std::mem::take(&mut cur));
                                }
                            }
                            _ => cur.push(c),
                        }
                    }
                    if !cur.is_empty() {
                        v.push(cur);
                    }
                }
                _ => {}
            }
        }
    }
    v
}

/// btrfs: 같은 파일시스템에 속한 모든 장치 (/sys/fs/btrfs/<UUID>/devices/<커널 이름>)
fn btrfs_members(any_member: &str) -> Vec<String> {
    let Ok(rd) = fs::read_dir("/sys/fs/btrfs") else { return Vec::new() };
    for fsdir in rd.flatten() {
        let devs: Vec<String> = fs::read_dir(fsdir.path().join("devices"))
            .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        if devs.iter().any(|d| d == any_member) {
            return devs;
        }
    }
    Vec::new()
}

/// ZFS: 풀 구성 장치 (udev가 zfs_member 파티션에 풀 이름을 ID_FS_LABEL로 붙임). udev가 없으면 찾지 못함
fn zfs_members(pool: &str) -> Vec<String> {
    let Ok(rd) = fs::read_dir("/sys/class/block") else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let u = udev(dev_of(&n)?);
            (u.get("ID_FS_TYPE").map(String::as_str) == Some("zfs_member") && u.get("ID_FS_LABEL").map(|l| udev_unescape(l)) == Some(pool.to_string())).then_some(n)
        })
        .collect()
}

/// 마운트가 실제로 쓰는 블록 장치들의 커널 이름.
/// 보통은 하나지만 btrfs 여러 장치, ZFS 풀은 여러 개, overlay는 아래 경로들의 장치
fn mount_devices(m: &Mount, all: &[Mount], depth: u32) -> Vec<String> {
    if depth > 8 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    match m.fstype.as_str() {
        "overlay" => {
            for d in overlay_dirs(&m.opts) {
                if let Some(under) = mount_of(Path::new(&d), all).filter(|u| u.point != m.point) {
                    for n in mount_devices(&under, all, depth + 1) {
                        if !out.contains(&n) {
                            out.push(n);
                        }
                    }
                }
            }
        }
        "zfs" => out = zfs_members(m.source.split('/').next().unwrap_or("")),
        _ => {
            if let Some(n) = mount_block_dev(m).and_then(name_of) {
                if m.fstype == "btrfs" {
                    out = btrfs_members(&n);
                }
                if !out.contains(&n) {
                    out.insert(0, n);
                }
            }
        }
    }
    out
}

/// 경로를 실제 위치로 풂. 아직 없는 경로(만들 증거 폴더 등)는 있는 가장 가까운 조상을 풀고 나머지를 붙임
/// (조상이 다른 디스크를 가리키는 링크면 그 디스크로 판정되게)
fn canonical_or_ancestor(p: &Path) -> PathBuf {
    if let Ok(r) = fs::canonicalize(p) {
        return r;
    }
    // 상대 경로는 현재 폴더 기준 절대 경로로 (그러지 않으면 어느 마운트와도 맞지 않아 /로 판정됨)
    let p = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.to_path_buf()) };
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.clone();
    while let Some(parent) = cur.parent().map(Path::to_path_buf) {
        if let Some(name) = cur.file_name() {
            rest.push(name.to_os_string());
        }
        if let Ok(mut r) = fs::canonicalize(&parent) {
            for n in rest.iter().rev() {
                r.push(n);
            }
            return r;
        }
        cur = parent;
    }
    p
}

/// 저장 위치가 쓰는 블록 장치를 확인할 수 있는지 (못 하면 겹침 검사를 할 수 없음 → 화면, 로그 경고)
pub fn output_devices_known(p: &Path) -> bool {
    let all = mounts();
    match mount_of(p, &all) {
        Some(m) if drive_type(&m.fstype) != "fixed" => true, // 네트워크, 메모리: 디스크와 겹칠 일 없음
        Some(m) => !mount_devices(&m, &all, 0).is_empty(),
        None => false,
    }
}

/// 경로가 속한 마운트 (가장 긴 마운트 위치 일치)
fn mount_of(p: &Path, all: &[Mount]) -> Option<Mount> {
    let real = canonical_or_ancestor(p);
    let s = real.display().to_string();
    all.iter()
        .filter(|m| {
            let mp = m.point.trim_end_matches('/');
            mp.is_empty() || s == mp || s.starts_with(&format!("{mp}/"))
        })
        .max_by_key(|m| m.point.len())
        .cloned()
}

fn statvfs(p: &str) -> Option<(u64, u64)> {
    let c = CString::new(p).ok()?;
    // SAFETY: 유효한 C 문자열과 출력 구조체
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return None;
        }
        let fr = s.f_frsize as u64;
        Some((s.f_blocks as u64 * fr, s.f_bavail as u64 * fr))
    }
}

fn drive_type(fstype: &str) -> &'static str {
    match fstype {
        "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "fuse.sshfs" | "9p" | "ceph" | "glusterfs" | "fuse.glusterfs" | "davfs" | "fuse.rclone" => "remote",
        "tmpfs" | "ramfs" => "ram",
        _ => "fixed",
    }
}

// -- 블록 장치 -------------------------------------------------------------

fn sys_block(name: &str) -> PathBuf {
    Path::new("/sys/class/block").join(name)
}

fn dev_of(name: &str) -> Option<(u32, u32)> {
    parse_dev(&read_trim(sys_block(name).join("dev"))?)
}

/// 장치 번호 → 커널 이름 ("8:1" → "sda1")
fn name_of(dev: (u32, u32)) -> Option<String> {
    let link = fs::read_link(format!("/sys/dev/block/{}:{}", dev.0, dev.1)).ok()?;
    Some(link.file_name()?.to_string_lossy().into_owned())
}

/// 파티션이면 상위 디스크 이름, 아니면 그대로
fn whole_disk(name: &str) -> String {
    let p = sys_block(name);
    if p.join("partition").exists() {
        if let Ok(real) = fs::canonicalize(&p) {
            if let Some(parent) = real.parent().and_then(|x| x.file_name()) {
                return parent.to_string_lossy().into_owned();
            }
        }
    }
    name.to_string()
}

fn slaves(name: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(sys_block(name).join("slaves"))
        .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// udev 데이터베이스의 속성 (E:KEY=VALUE). 컨테이너, 최소 설치 등 udev가 없으면 비어 있음
fn udev(dev: (u32, u32)) -> HashMap<String, String> {
    fs::read_to_string(format!("/run/udev/data/b{}:{}", dev.0, dev.1))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("E:"))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// udev 값의 \x20 같은 이스케이프 풀기
fn udev_unescape(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            // 바이트 단위로 해석 (멀티바이트 문자 경계에서 문자열 슬라이스 패닉 방지)
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 2]), hex(b[i + 3])) {
                out.push((h * 16 + l) as u8);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn gpt_kind(guid: &str) -> &'static str {
    match guid.to_ascii_lowercase().as_str() {
        "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" => crate::tr!("EFI 시스템", "EFI System"),
        "21686148-6449-6e6f-744e-656564454649" => crate::tr!("BIOS 부트", "BIOS Boot"),
        "0fc63daf-8483-4772-8e79-3d69d8477de4" => crate::tr!("Linux 파일시스템", "Linux Filesystem"),
        "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f" => crate::tr!("Linux 스왑", "Linux Swap"),
        "e6d6d379-f507-44c2-a23c-238f2a3df928" => "Linux LVM",
        "a19d880f-05fc-4d3b-a006-743f0f84911e" => "Linux RAID",
        "ca7d7ccb-63ed-4c53-861c-1742536059cc" => "Linux LUKS",
        "4f68bce3-e8cd-4db1-96e7-fbcaf984b709" => crate::tr!("Linux 루트 (x86-64)", "Linux Root (x86-64)"),
        "b921b045-1df0-41c3-af44-4c6f280d3fae" => crate::tr!("Linux 루트 (ARM64)", "Linux Root (ARM64)"),
        "933ac7e1-2eb4-4f13-b844-0e14e2aef915" => "Linux /home",
        "bc13c2ff-59e6-4262-a352-b275fd6f7172" => "Linux /boot",
        "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7" => crate::tr!("기본 데이터", "Basic Data"),
        "e3c9e316-0b5c-4db8-817d-f92df00215ae" => crate::tr!("Microsoft 예약", "Microsoft Reserved"),
        "de94bba4-06d1-4d40-a16a-bfd50179d6ac" => crate::tr!("복구", "Recovery"),
        _ => "",
    }
}

fn mbr_kind(t: &str) -> &'static str {
    match t.trim_start_matches("0x").to_ascii_lowercase().as_str() {
        "83" => "Linux",
        "82" => crate::tr!("Linux 스왑", "Linux Swap"),
        "8e" => "Linux LVM",
        "fd" => "Linux RAID",
        "ef" => crate::tr!("EFI 시스템", "EFI System"),
        "5" | "05" | "f" | "0f" | "85" => crate::tr!("확장", "Extended"),
        "7" | "07" => "NTFS/exFAT",
        "b" | "0b" | "c" | "0c" => "FAT32",
        "ee" => crate::tr!("GPT 보호", "GPT Protective"),
        _ => "",
    }
}

fn bus_of(name: &str, sys_real: &str, u: &HashMap<String, String>) -> String {
    if name.starts_with("dm-") {
        let uuid = read_trim(sys_block(name).join("dm/uuid")).unwrap_or_default();
        return if uuid.starts_with("LVM-") {
            "LVM"
        } else if uuid.starts_with("CRYPT-") {
            "LUKS"
        } else if uuid.starts_with("mpath-") {
            crate::tr!("멀티패스", "Multipath")
        } else {
            "device-mapper"
        }
        .into();
    }
    if name.starts_with("md") {
        return "RAID".into();
    }
    if name.starts_with("loop") {
        return "loop".into();
    }
    if name.starts_with("nbd") {
        return "NBD".into();
    }
    if sys_real.contains("/usb") {
        return "USB".into();
    }
    if name.starts_with("nvme") {
        return "NVMe".into();
    }
    if sys_real.contains("/virtio") {
        return "VirtIO".into();
    }
    if name.starts_with("xvd") || sys_real.contains("/xen") {
        return "Xen".into();
    }
    if name.starts_with("mmcblk") {
        return "SD".into();
    }
    match u.get("ID_BUS").map(String::as_str) {
        Some("ata") => "SATA".into(),
        Some("usb") => "USB".into(),
        Some("scsi") if sys_real.contains("/ata") => "SATA".into(),
        _ if sys_real.contains("/ata") => "SATA".into(),
        _ => "SCSI".into(),
    }
}

fn model_of(name: &str, u: &HashMap<String, String>) -> String {
    let b = sys_block(name);
    if name.starts_with("dm-") {
        let dm = read_trim(b.join("dm/name")).unwrap_or_default();
        let uuid = read_trim(b.join("dm/uuid")).unwrap_or_default();
        return if uuid.starts_with("LVM-") {
            crate::tr!(format!("LVM 논리 볼륨 {dm}"), format!("LVM logical volume {dm}"))
        } else if uuid.starts_with("CRYPT-") {
            crate::tr!(format!("암호화 볼륨 (LUKS) {dm}"), format!("Encrypted volume (LUKS) {dm}"))
        } else {
            format!("device-mapper {dm}")
        };
    }
    if name.starts_with("md") {
        let level = read_trim(b.join("md/level")).unwrap_or_default();
        return crate::tr!(format!("소프트웨어 RAID {name} ({level})"), format!("Software RAID {name} ({level})"));
    }
    if name.starts_with("loop") {
        let f = read_trim(b.join("loop/backing_file")).unwrap_or_default();
        return crate::tr!(format!("루프 장치 ({f})"), format!("Loop device ({f})"));
    }
    // virtio 디스크(KVM, 클라우드)는 device/vendor가 PCI 번호(0x1af4)뿐이고 모델이 없음
    if name.starts_with("vd") {
        return crate::tr!("VirtIO 가상 디스크", "VirtIO virtual disk").into();
    }
    let vendor = read_trim(b.join("device/vendor")).unwrap_or_default();
    let model = read_trim(b.join("device/model")).or_else(|| u.get("ID_MODEL").map(|m| m.replace('_', " "))).unwrap_or_default();
    let s = if !vendor.is_empty() && !model.starts_with(&vendor) && vendor != "ATA" { format!("{vendor} {model}") } else { model };
    let s = s.trim().to_string();
    if s.is_empty() {
        name.to_string()
    } else {
        s
    }
}

fn serial_of(name: &str, u: &HashMap<String, String>) -> String {
    let b = sys_block(name);
    if name.starts_with("dm-") {
        return read_trim(b.join("dm/uuid")).unwrap_or_default();
    }
    u.get("ID_SERIAL_SHORT")
        .cloned()
        .or_else(|| read_trim(b.join("device/serial")))
        // virtio 디스크: 가상화 설정의 시리얼이 /sys/block/vdX/serial에 있음 (udev는 ID_SERIAL에만 넣기도 함)
        .or_else(|| read_trim(b.join("serial")))
        .or_else(|| u.get("ID_SERIAL").cloned())
        // udev가 없는 시스템(최소 설치, initramfs, 컨테이너)의 SATA, SAS, USB 디스크: 커널이 읽어 둔 VPD 0x80(장치 시리얼)
        .or_else(|| fs::read(b.join("device/vpd_pg80")).ok().and_then(|v| vpd80_serial(&v)))
        .or_else(|| u.get("MD_UUID").cloned())
        .or_else(|| read_trim(b.join("device/wwid")))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// SCSI VPD 0x80 (Unit Serial Number): [0]=장치 종류, [1]=0x80, [2..4]=길이, [4..]=ASCII 시리얼
fn vpd80_serial(v: &[u8]) -> Option<String> {
    if v.len() < 4 || v[1] != 0x80 {
        return None;
    }
    let len = u16::from_be_bytes([v[2], v[3]]) as usize;
    let body = v.get(4..4 + len.min(v.len() - 4))?;
    let s: String = body.iter().filter(|b| b.is_ascii_graphic() || **b == b' ').map(|b| *b as char).collect();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// 목록에 넣을 장치인지
fn wanted(name: &str) -> bool {
    const SKIP: [&str; 5] = ["ram", "zram", "sr", "fd", "mtdblock"];
    if SKIP.iter().any(|p| name.starts_with(p)) {
        return false;
    }
    let b = Path::new("/sys/block").join(name);
    if read_trim(b.join("hidden")).as_deref() == Some("1") {
        return false;
    }
    if read_u64(b.join("size")).unwrap_or(0) == 0 {
        return false;
    }
    if name.starts_with("loop") {
        // snap 패키지 루프 장치는 목록을 어지럽히므로 숨김 (목록에 없는 장치는 이미징 대상으로 지정할 수 없음)
        let f = read_trim(b.join("loop/backing_file")).unwrap_or_default();
        if f.contains("/snapd/") || f.ends_with(".snap") {
            return false;
        }
    }
    true
}

fn sort_key(name: &str) -> (u8, String) {
    let group = if name.starts_with("md") {
        1
    } else if name.starts_with("dm-") {
        2
    } else if name.starts_with("loop") || name.starts_with("nbd") {
        3
    } else {
        0
    };
    // sda < sdb < sdaa, nvme0n1 < nvme1n1
    (group, format!("{:>12}", name))
}

pub fn list_disks() -> Result<Vec<DiskInfo>> {
    let all_mounts = mounts();
    let mut by_dev: HashMap<(u32, u32), Vec<&Mount>> = HashMap::new();
    for m in &all_mounts {
        if let Some(d) = mount_block_dev(m) {
            by_dev.entry(d).or_default().push(m);
        }
    }
    let points = |dev: Option<(u32, u32)>| -> Vec<&Mount> {
        let mut v = dev.and_then(|d| by_dev.get(&d)).cloned().unwrap_or_default();
        v.sort_by_key(|m| m.point.len());
        v
    };

    let mut names: Vec<String> = fs::read_dir("/sys/block")
        .context(crate::tr!("/sys/block을 읽을 수 없습니다", "Cannot read /sys/block"))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| wanted(n))
        .collect();
    names.sort_by_key(|n| sort_key(n));

    let mut disks = Vec::new();
    for (idx, name) in names.iter().enumerate() {
        let b = sys_block(name);
        let dev = dev_of(name);
        let u = dev.map(udev).unwrap_or_default();
        let sys_real = fs::canonicalize(&b).map(|p| p.display().to_string()).unwrap_or_default();
        let size = read_u64(b.join("size")).unwrap_or(0) * 512;
        let sector = read_u64(b.join("queue/logical_block_size")).unwrap_or(512) as u32;

        let mut parts = Vec::new();
        if let Ok(rd) = fs::read_dir(&b) {
            for e in rd.flatten() {
                let pn = e.file_name().to_string_lossy().into_owned();
                let pp = e.path();
                let Some(number) = read_u64(pp.join("partition")) else { continue };
                let pdev = dev_of(&pn);
                let pu = pdev.map(udev).unwrap_or_default();
                let mts = points(pdev);
                let first = mts.first();
                let kind = pu
                    .get("ID_PART_ENTRY_TYPE")
                    .map(|t| if t.contains('-') { gpt_kind(t) } else { mbr_kind(t) })
                    .unwrap_or("")
                    .to_string();
                parts.push(PartitionInfo {
                    number: number as u32,
                    offset: read_u64(pp.join("start")).unwrap_or(0) * 512,
                    size: read_u64(pp.join("size")).unwrap_or(0) * 512,
                    kind,
                    name: pu.get("ID_PART_ENTRY_NAME").map(|s| udev_unescape(s)).unwrap_or_default(),
                    letter: first.map(|m| m.point.clone()),
                    device: Some(format!("/dev/{pn}")),
                    label: pu.get("ID_FS_LABEL_ENC").map(|s| udev_unescape(s)).or_else(|| pu.get("ID_FS_LABEL").cloned()).unwrap_or_default(),
                    fs: first.map(|m| m.fstype.clone()).or_else(|| pu.get("ID_FS_TYPE").cloned()).unwrap_or_default(),
                    free: first.and_then(|m| statvfs(&m.point)).map(|(_, f)| f),
                });
            }
        }
        parts.sort_by_key(|p| p.number);

        let whole_mounts = points(dev);
        let style = match u.get("ID_PART_TABLE_TYPE").map(String::as_str) {
            Some("gpt") => "GPT".to_string(),
            Some("dos") => "MBR".to_string(),
            Some(o) => o.to_ascii_uppercase(),
            None if !parts.is_empty() => crate::tr!("파티션 있음", "partitioned").into(),
            None => crate::tr!("없음", "none").into(),
        };
        let path = if name.starts_with("dm-") {
            read_trim(b.join("dm/name")).map(|n| format!("/dev/mapper/{n}")).unwrap_or_else(|| format!("/dev/{name}"))
        } else {
            format!("/dev/{name}")
        };
        disks.push(DiskInfo {
            number: idx as u32,
            name: name.clone(),
            path,
            model: model_of(name, &u),
            serial: serial_of(name, &u),
            bus: bus_of(name, &sys_real, &u),
            removable: read_trim(b.join("removable")).as_deref() == Some("1"),
            size,
            sector_size: sector,
            partition_style: style,
            partitions: parts,
            roles: Vec::new(),
            mounts: whole_mounts.iter().map(|m| m.point.clone()).collect(),
            fs: whole_mounts.first().map(|m| m.fstype.clone()).or_else(|| u.get("ID_FS_TYPE").cloned()).unwrap_or_default(),
            backing: slaves(name),
        });
    }
    if disks.is_empty() {
        return Err(anyhow!(crate::tr!(
            "블록 장치를 찾지 못했습니다 (/sys/block이 비어 있음). 컨테이너 안에서 실행 중인지 확인하세요",
            "No block devices found (/sys/block is empty). Check whether you are running inside a container"
        )));
    }
    Ok(disks)
}

/// 경로가 저장되는 디스크 번호들 (LVM, LUKS, RAID면 아래 물리 디스크까지 포함)
pub fn disks_for_path(p: &Path, disks: &[DiskInfo]) -> Vec<u32> {
    let all = mounts();
    let Some(m) = mount_of(p, &all) else { return Vec::new() };
    // 겹침 판정(stack_names)과 같은 장치 묶음: 루프 장치 원본 파일이 있는 디스크까지
    let mut stack: Vec<String> = Vec::new();
    for name in mount_devices(&m, &all, 0) {
        stack.extend(stack_names(&name));
    }
    disks.iter().filter(|d| stack.contains(&d.name)).map(|d| d.number).collect()
}

/// 장치와 그 아래 모든 장치의 커널 이름 (파티션, dm 자체와 그 디스크 모두). 예: dm-1 → dm-0 → sda3 → sda
fn stack_names(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut todo = vec![name.to_string()];
    while let Some(n) = todo.pop() {
        if out.contains(&n) || out.len() > 128 {
            continue;
        }
        let disk = whole_disk(&n);
        todo.extend(slaves(&n));
        // 루프 장치는 원본 파일이 있는 장치까지 따라감 (그 장치를 이미징하면 저장 파일도 그 안에 써짐)
        if disk.starts_with("loop") {
            if let Some(f) = read_trim(sys_block(&disk).join("loop/backing_file")) {
                let all = mounts();
                if let Some(m) = mount_of(Path::new(f.trim_end_matches(" (deleted)")), &all) {
                    todo.extend(mount_devices(&m, &all, 0));
                }
            }
        }
        // 디스크도 다시 펼침: md0p1 → md0 → sda, sdb처럼 디스크 자체가 다른 장치 위에 있을 수 있음
        if disk != n {
            todo.push(disk);
        }
        out.push(n);
    }
    out
}

/// 장치 경로의 커널 이름 (/dev/mapper/vg-root → dm-1)
fn name_of_path(p: &str) -> Option<String> {
    let rdev = fs::metadata(p).ok()?.rdev();
    name_of((libc::major(rdev), libc::minor(rdev)))
}

/// 이미징 대상이 저장 폴더가 있는 장치와 겹치는지.
/// 저장 폴더 장치 묶음(파티션, LVM, LUKS, 그 디스크)에 대상이 들어 있으면 겹침.
/// 같은 디스크라도 다른 파티션, 다른 논리 볼륨은 겹치지 않음
pub fn overlaps_output(t: &Target, disks: &[DiskInfo], out: &Path) -> bool {
    let all = mounts();
    let Some(m) = mount_of(out, &all) else { return false };
    let mut used: Vec<String> = Vec::new();
    for name in mount_devices(&m, &all, 0) {
        used.extend(stack_names(&name));
    }
    let path = match t {
        Target::Device { path, .. } => path.clone(),
        Target::Disk { number, .. } => match disks.iter().find(|d| d.number == *number) {
            Some(d) => d.path.clone(),
            None => return false,
        },
        Target::Partition { disk, number, .. } => {
            match disks.iter().find(|d| d.number == *disk).and_then(|d| d.partitions.iter().find(|p| p.number == *number)).and_then(|p| p.device.clone()) {
                Some(p) => p,
                None => return false,
            }
        }
        Target::Volume { .. } | Target::File { .. } | Target::Memory => return false,
    };
    name_of_path(&path).is_some_and(|n| used.contains(&n))
}

/// 루트(/)를 담고 있는 디스크 번호들
pub fn system_disks(disks: &[DiskInfo]) -> Vec<u32> {
    disks_for_path(Path::new("/"), disks)
}

/// 경로가 속한 마운트 정보. `letter`에는 마운트 위치가 들어갑니다
pub fn volume_for_path(p: &Path) -> Option<VolumeInfo> {
    let all = mounts();
    let m = mount_of(p, &all)?;
    let (size, free) = statvfs(&m.point).unwrap_or((0, 0));
    let dev = mount_block_dev(&m);
    let label = dev.map(udev).and_then(|u| u.get("ID_FS_LABEL").cloned()).unwrap_or_default();
    let disk_number = list_disks().ok().and_then(|ds| disks_for_path(p, &ds).first().copied());
    Some(VolumeInfo {
        letter: m.point.clone(),
        label,
        fs: m.fstype.clone(),
        size,
        free,
        disk_number,
        offset: 0,
        drive_type: drive_type(&m.fstype).into(),
    })
}

// -- 원본 열기 -------------------------------------------------------------

/// 블록 장치 원본.
/// - 읽기 오류가 나면 장치가 아직 있는지 확인: USB를 뽑으면 Linux는 EIO를 주는데, 이를 불량 섹터로 보고
///   오래 재시도하지 않도록 장치가 사라졌으면 ENODEV(즉시 중단)로 바꿔 알림
/// - O_DIRECT를 쓰지 못한 경우: 읽은 범위를 바로 페이지 캐시에서 버림
struct BlockSource {
    inner: FileSource,
    /// 캐시 버리기용 (O_DIRECT가 아닐 때만)
    cache_file: Option<File>,
    /// /sys/class/block/<이름> (장치 존재 확인)
    sys: PathBuf,
}

impl Source for BlockSource {
    fn len(&self) -> u64 {
        self.inner.len()
    }
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.read_at(offset, buf) {
            Ok(n) => {
                if let Some(f) = &self.cache_file {
                    drop_cache(f, offset, n as u64);
                }
                Ok(n)
            }
            // 장치가 사라짐 → 엔진이 즉시 중단하는 오류(ENODEV)로 알림
            Err(_) if !self.sys.as_os_str().is_empty() && !self.sys.exists() => Err(io::Error::from_raw_os_error(libc::ENODEV)),
            Err(e) => Err(e),
        }
    }
}

/// 장치의 논리 섹터 크기 (BLKSSZGET). 실패하면 None
fn ioctl_sector_size(f: &File) -> Option<u32> {
    const BLKSSZGET: libc::c_ulong = 0x1268;
    let mut v: libc::c_int = 0;
    // SAFETY: 유효한 fd, 출력은 int 하나
    let r = unsafe { libc::ioctl(f.as_raw_fd(), BLKSSZGET as _, &mut v) };
    (r == 0 && v >= 512).then_some(v as u32)
}

fn is_direct(f: &File) -> bool {
    // SAFETY: 유효한 파일 디스크립터
    let fl = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETFL) };
    fl >= 0 && (fl & libc::O_DIRECT) != 0
}

/// 블록 장치 경로를 열어 원본으로
fn open_block(path: &str) -> Result<Box<dyn Source>> {
    let md = fs::metadata(path).with_context(|| crate::tr!(format!("{path}을(를) 찾을 수 없습니다"), format!("{path} not found")))?;
    use std::os::unix::fs::FileTypeExt;
    if !md.file_type().is_block_device() {
        return Err(anyhow!(crate::tr!(format!("{path}은(는) 블록 장치가 아닙니다"), format!("{path} is not a block device"))));
    }
    let rdev = md.rdev();
    let name = name_of((libc::major(rdev), libc::minor(rdev))).unwrap_or_default();
    let mut f = open_device_readonly(path).with_context(|| crate::tr!(format!("{path} 열기 실패 (root 권한인지 확인하세요)"), format!("Failed to open {path} (check that you are root)")))?;
    // 섹터 크기: 장치에 직접 묻고(ioctl), 안 되면 sysfs. 4Kn 디스크를 512로 잘못 잡으면 O_DIRECT 정렬 오류
    let sector = ioctl_sector_size(&f)
        .or_else(|| (!name.is_empty()).then(|| read_u64(sys_block(&whole_disk(&name)).join("queue/logical_block_size"))).flatten().map(|v| v as u32))
        .unwrap_or(512);
    let len = f.seek(SeekFrom::End(0)).with_context(|| crate::tr!(format!("{path} 크기를 알 수 없습니다"), format!("Cannot determine the size of {path}")))?;
    let cache_file = if is_direct(&f) { None } else { Some(f.try_clone()?) };
    let sys = if name.is_empty() { PathBuf::new() } else { sys_block(&name) };
    Ok(Box::new(BlockSource { inner: FileSource::new(f, 0, len, sector), cache_file, sys }))
}

pub fn open_target(t: &Target, disks: &[DiskInfo]) -> Result<Box<dyn Source>> {
    match t {
        Target::Device { path, .. } => open_block(path),
        Target::Disk { number, .. } => {
            let d = disks.iter().find(|d| d.number == *number).ok_or_else(|| anyhow!(crate::tr!(format!("disk{number}을(를) 찾지 못했습니다"), format!("disk{number} not found"))))?;
            open_block(&d.path)
        }
        Target::Partition { disk, number, .. } => {
            let d = disks.iter().find(|d| d.number == *disk).ok_or_else(|| anyhow!(crate::tr!(format!("disk{disk}을(를) 찾지 못했습니다"), format!("disk{disk} not found"))))?;
            let p = d.partitions.iter().find(|p| p.number == *number).ok_or_else(|| anyhow!(crate::tr!(format!("disk{disk}의 파티션 {number}을(를) 찾지 못했습니다"), format!("Partition {number} on disk{disk} not found"))))?;
            open_block(p.device.as_deref().ok_or_else(|| anyhow!(crate::tr!("파티션 장치 경로가 없습니다", "Partition has no device path")))?)
        }
        Target::Volume { letter } => Err(anyhow!(crate::tr!(format!("Linux에서는 장치 경로로 지정하세요 (예: /dev/sda1). 입력: {letter}"), format!("On Linux, specify a device path (e.g. /dev/sda1). Input: {letter}")))),
        Target::File { path } => Ok(Box::new(FileSource::open_file(path)?)),
        Target::Memory => Err(anyhow!(crate::tr!("메모리는 memory 모듈로 엽니다", "Memory is opened by the memory module"))),
    }
}

// -- 프로세스 --------------------------------------------------------------

/// (pid, ppid, 이름, 시작 시각 틱)
fn proc_stat(pid: u32) -> Option<(u32, u32, String, u64)> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let name = s[open + 1..close].to_string();
    let rest: Vec<&str> = s.get(close + 2..)?.split(' ').collect();
    // rest[0] = state, rest[1] = ppid, rest[19] = starttime
    Some((pid, rest.get(1)?.parse().ok()?, name, rest.get(19)?.parse().ok()?))
}

pub fn process_tree(root: u32) -> Vec<ProcInfo> {
    let all: Vec<(u32, u32, String, u64)> = fs::read_dir("/proc")
        .map(|r| r.flatten().filter_map(|e| e.file_name().to_str().and_then(|s| s.parse::<u32>().ok())).filter_map(proc_stat).collect())
        .unwrap_or_default();
    // 부모 PID는 재사용될 수 있으므로, 부모보다 늦게 시작한 프로세스만 자식으로 인정
    let mut out: Vec<(u32, u32, String, u64)> = all.iter().filter(|p| p.0 == root).cloned().collect();
    let mut i = 0;
    while i < out.len() {
        let (pid, start) = (out[i].0, out[i].3);
        for c in all.iter().filter(|p| p.1 == pid && p.0 != pid && p.3 >= start) {
            if !out.iter().any(|o| o.0 == c.0) {
                out.push(c.clone());
            }
        }
        i += 1;
    }
    out.into_iter().map(|(pid, parent_pid, name, _)| ProcInfo { pid, parent_pid, name }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_option_dirs() {
        let o = "rw, lowerdir=/lower1:/lower2,upperdir=/mnt/usb/up, workdir=/mnt/usb/work, xino=off";
        assert_eq!(overlay_dirs(o), vec!["/lower1", "/lower2", "/mnt/usb/up", "/mnt/usb/work"]);
        assert_eq!(overlay_dirs("lowerdir=/a\\:b:/c"), vec!["/a:b", "/c"]);
        // 경로 안 쉼표는 mountinfo에서 \054로 나옴
        assert_eq!(overlay_dirs("upperdir=/mnt/a\\054b, workdir=/w"), vec!["/mnt/a, b", "/w"]);
        assert!(overlay_dirs("rw, relatime").is_empty());
    }

    #[test]
    fn vpd80_parse() {
        let mut v = vec![0x00, 0x80, 0x00, 0x0c];
        v.extend_from_slice(b"  WD-1234ABCD");
        assert_eq!(vpd80_serial(&v).as_deref(), Some("WD-1234ABC"));
        assert_eq!(vpd80_serial(&[0, 0x83, 0, 0]), None);
        assert_eq!(vpd80_serial(&[0, 0x80, 0, 4, b' ', b' ', 0, 0]), None);
    }

    #[test]
    fn unescape_mountinfo() {
        assert_eq!(unescape(r"/mnt/my\040usb"), "/mnt/my usb");
        assert_eq!(unescape("/plain"), "/plain");
        assert_eq!(udev_unescape(r"MY\x20DISK"), "MY DISK");
    }

    #[test]
    fn mount_longest_prefix() {
        let m = |p: &str| Mount { dev: (8, 1), point: p.into(), fstype: "ext4".into(), source: "/dev/sda1".into(), opts: String::new() };
        let all = vec![m("/"), m("/mnt"), m("/mnt/usb"), m("/mnt/usb2")];
        assert_eq!(mount_of(Path::new("/mnt/usb/cases"), &all).unwrap().point, "/mnt/usb");
        assert_eq!(mount_of(Path::new("/mnt/usb2"), &all).unwrap().point, "/mnt/usb2");
        assert_eq!(mount_of(Path::new("/etc"), &all).unwrap().point, "/");
    }

    #[test]
    fn this_system() {
        // 실제 시스템에서 기본 정보가 읽히는지 (root가 아니어도 됨)
        let h = host_info();
        assert!(!h.hostname.is_empty());
        assert!(!process_tree(std::process::id()).is_empty());
    }
}
