//! 이미징 대상 지정과 해석.
//!
//! CLI 입력 예: `disk0`, `0`, `\\.\PhysicalDrive0`, `C:`, `disk0p3`(문자 없는 파티션), 일반 파일 경로
//! CLI 입력 예 (Linux): `sda`, `/dev/sda`, `nvme0n1p2`, `/dev/mapper/vg-root`, `md0`, `disk0`

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Target {
    /// 물리 디스크 전체. `serial`이 있으면 시작할 때 같은 디스크인지 확인
    Disk {
        number: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial: Option<String>,
    },
    /// 드라이브 문자가 있는 볼륨 (BitLocker 해제된 내용으로 읽힘)
    Volume { letter: String },
    /// 문자 없는 파티션: 물리 디스크의 해당 범위로 읽음
    Partition {
        disk: u32,
        number: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial: Option<String>,
    },
    /// 블록 장치 경로 (Linux: 디스크, 파티션, LVM, RAID 모두). `serial`은 디스크 시리얼 확인용
    Device {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial: Option<String>,
    },
    /// 일반 파일 (이미지 재수집, 시험용)
    File { path: PathBuf },
    /// 물리 메모리 (LiME 형식으로 저장, Linux)
    Memory,
}

/// Linux 블록 장치 이름처럼 보이는지 ("sda", "nvme0n1p2", "mapper/vg-root" 등)
#[cfg(unix)]
fn looks_like_block_name(s: &str) -> bool {
    const PREFIX: [&str; 11] = ["sd", "nvme", "vd", "xvd", "hd", "mmcblk", "md", "dm-", "loop", "mapper/", "nbd"];
    PREFIX.iter().any(|p| s.starts_with(p)) && !s.contains("..") && s.len() < 128
}

impl Target {
    pub fn parse(s: &str) -> Option<Target> {
        let t = s.trim();
        if matches!(t.to_ascii_lowercase().as_str(), "memory" | "mem" | "ram") {
            return Some(Target::Memory);
        }
        #[cfg(unix)]
        {
            if let Some(rest) = t.strip_prefix("/dev/") {
                if !rest.is_empty() && !rest.contains("..") {
                    return Some(Target::Device { path: t.to_string(), serial: None });
                }
            }
            if looks_like_block_name(t) && std::path::Path::new("/dev").join(t).exists() {
                return Some(Target::Device { path: format!("/dev/{t}"), serial: None });
            }
        }
        let lower = t.to_ascii_lowercase();
        if let Some(n) = lower.strip_prefix(r"\\.\physicaldrive") {
            return n.parse().ok().map(|number| Target::Disk { number, serial: None });
        }
        if let Some(rest) = lower.strip_prefix("disk") {
            if let Some((d, p)) = rest.split_once('p') {
                return Some(Target::Partition { disk: d.parse().ok()?, number: p.parse().ok()?, serial: None });
            }
            return rest.parse().ok().map(|number| Target::Disk { number, serial: None });
        }
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            return t.parse().ok().map(|number| Target::Disk { number, serial: None });
        }
        let vol = lower.strip_prefix(r"\\.\").unwrap_or(&lower);
        let vol = vol.trim_end_matches('\\');
        if vol.len() == 2 && vol.ends_with(':') && vol.as_bytes()[0].is_ascii_alphabetic() {
            return Some(Target::Volume { letter: vol.to_ascii_uppercase() });
        }
        let p = PathBuf::from(t);
        if p.is_file() {
            return Some(Target::File { path: p });
        }
        None
    }

    /// 파일 이름 꼬리표: Disk0, C, Disk0_P3
    pub fn tag(&self) -> String {
        match self {
            Target::Disk { number, .. } => format!("Disk{number}"),
            Target::Volume { letter } => letter.trim_end_matches(':').to_string(),
            Target::Partition { disk, number, .. } => format!("Disk{disk}_P{number}"),
            Target::Device { path, .. } => {
                let p = path.strip_prefix("/dev/").unwrap_or(path);
                let p = p.strip_prefix("mapper/").unwrap_or(p);
                let s: String = p.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
                if s.is_empty() { "Device".into() } else { s }
            }
            Target::Memory => "Memory".into(),
            Target::File { path: p } => p
                .file_stem()
                .map(|s| s.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect())
                .unwrap_or_else(|| "File".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_forms() {
        assert_eq!(Target::parse("disk0"), Some(Target::Disk { number: 0, serial: None }));
        assert_eq!(Target::parse("3"), Some(Target::Disk { number: 3, serial: None }));
        assert_eq!(Target::parse(r"\\.\PhysicalDrive12"), Some(Target::Disk { number: 12, serial: None }));
        assert_eq!(Target::parse("c:"), Some(Target::Volume { letter: "C:".into() }));
        assert_eq!(Target::parse(r"\\.\D:"), Some(Target::Volume { letter: "D:".into() }));
        assert_eq!(Target::parse("disk1p3"), Some(Target::Partition { disk: 1, number: 3, serial: None }));
        assert_eq!(Target::parse("nope-not-a-file"), None);
        assert_eq!(Target::Partition { disk: 0, number: 4, serial: None }.tag(), "Disk0_P4");
        assert_eq!(Target::Volume { letter: "C:".into() }.tag(), "C");
        assert_eq!(Target::Device { path: "/dev/sda".into(), serial: None }.tag(), "sda");
        assert_eq!(Target::Device { path: "/dev/mapper/vg-root".into(), serial: None }.tag(), "vg-root");
        assert_eq!(Target::Device { path: "/dev/disk/by-id/ata-X:1".into(), serial: None }.tag(), "disk_by-id_ata-X_1");
    }
}
