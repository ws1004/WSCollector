//! macOS 등 구현 자리 (다음 차수). 지금은 기본 정보와 파일 원본만 지원합니다.

use anyhow::{anyhow, Result};

use crate::model::{DiskInfo, HostInfo, ProcInfo, VolumeInfo};
use crate::source::Source;
use crate::target::Target;

pub fn host_info() -> HostInfo {
    HostInfo {
        hostname: std::env::var("HOSTNAME").unwrap_or_default(),
        arch: std::env::consts::ARCH.into(),
        os_name: std::env::consts::OS.into(),
        ..Default::default()
    }
}

pub fn is_elevated() -> bool {
    false
}

pub fn list_disks() -> Result<Vec<DiskInfo>> {
    Err(anyhow!(crate::tr!("이 운영체제의 디스크 열거는 아직 지원하지 않습니다", "Disk enumeration is not yet supported on this operating system")))
}

pub fn volume_for_path(_p: &std::path::Path) -> Option<VolumeInfo> {
    None
}

pub fn system_letter() -> Option<String> {
    None
}

pub fn open_target(t: &Target, _disks: &[DiskInfo]) -> Result<Box<dyn Source>> {
    match t {
        Target::File { path: p } => Ok(Box::new(crate::source::FileSource::open_file(p)?)),
        _ => Err(anyhow!(crate::tr!("이 운영체제의 장치 읽기는 아직 지원하지 않습니다", "Device reading is not yet supported on this operating system"))),
    }
}

pub fn process_tree(_root: u32) -> Vec<ProcInfo> {
    Vec::new()
}

pub fn disks_for_path(_p: &std::path::Path, _disks: &[DiskInfo]) -> Vec<u32> {
    Vec::new()
}

pub fn system_disks(_disks: &[DiskInfo]) -> Vec<u32> {
    Vec::new()
}

pub fn overlaps_output(_t: &Target, _disks: &[DiskInfo], _out: &std::path::Path) -> bool {
    false
}

pub fn footprint_notes() -> Vec<String> {
    Vec::new()
}

pub const ADMIN_HINT: super::Bilingual = super::Bilingual { ko: "관리자(root) 권한이 필요합니다.", en: "Administrator (root) privileges are required." };

/// 저장 위치가 쓰는 디스크를 확인할 수 있는지 (Linux 전용 판별. 여기서는 늘 확인된 것으로 봄)
pub fn output_devices_known(_p: &std::path::Path) -> bool {
    true
}
