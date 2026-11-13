//! GUI, CLI가 공유하는 데이터 구조.
//! u64는 JS 정밀도 한계 때문에 JSON에서 문자열로 내보냅니다.

use serde::{Deserialize, Serialize};

pub mod u64_str {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

pub mod opt_u64_str {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(x) => s.serialize_some(&x.to_string()),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        s.map(|x| x.parse().map_err(serde::de::Error::custom)).transpose()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub os_build: String,
    pub arch: String,
    pub cpu: String,
    #[serde(with = "u64_str")]
    pub ram_bytes: u64,
    pub manufacturer: String,
    pub model: String,
    pub boot_time: String,
    pub local_time: String,
    pub timezone: String,
}

impl HostInfo {
    /// "빌드 22621.2861" (Windows) / "커널 6.8.0-45-generic" (Linux)
    pub fn build_label(&self) -> String {
        if self.os_name.contains("Windows") {
            crate::tr!(format!("빌드 {}", self.os_build), format!("build {}", self.os_build))
        } else {
            crate::tr!(format!("커널 {}", self.os_build), format!("kernel {}", self.os_build))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionInfo {
    pub number: u32,
    #[serde(with = "u64_str")]
    pub offset: u64,
    #[serde(with = "u64_str")]
    pub size: u64,
    /// "EFI 시스템", "기본 데이터", "복구" 등
    pub kind: String,
    /// GPT 파티션 이름
    pub name: String,
    /// Windows: 드라이브 문자 ("C:"), Linux: 마운트 위치 ("/boot")
    pub letter: Option<String>,
    /// Linux: 파티션 장치 경로 ("/dev/sda1"). Windows는 없음
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub label: String,
    pub fs: String,
    #[serde(with = "opt_u64_str")]
    pub free: Option<u64>,
}

impl PartitionInfo {
    /// 화면 표시 이름: "C: Windows", "EFI 시스템", "파티션 3"
    pub fn display_name(&self) -> String {
        match (&self.letter, self.label.is_empty()) {
            (Some(l), false) => format!("{l} {}", self.label),
            (Some(l), true) => l.clone(),
            (None, _) if !self.kind.is_empty() => self.kind.clone(),
            _ => crate::tr!(format!("파티션 {}", self.number), format!("Partition {}", self.number)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskRole {
    /// WS Collector가 실행 중인 매체
    Tool,
    /// 증거 파일 저장 매체
    Output,
    /// 대상 PC의 시스템 디스크 (Windows 디스크, Linux 루트 / 디스크)
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub number: u32,
    /// 커널 장치 이름 (Linux: "sda", "nvme0n1", "dm-0"). Windows는 빈 값
    #[serde(default)]
    pub name: String,
    pub path: String,
    pub model: String,
    pub serial: String,
    pub bus: String,
    pub removable: bool,
    #[serde(with = "u64_str")]
    pub size: u64,
    pub sector_size: u32,
    pub partition_style: String,
    pub partitions: Vec<PartitionInfo>,
    pub roles: Vec<DiskRole>,
    /// Linux: 장치 전체가 바로 마운트된 위치 (LVM, LUKS 볼륨 등)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<String>,
    /// Linux: 장치 전체의 파일시스템 (LVM, LUKS 볼륨 등)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fs: String,
    /// Linux: 이 장치를 이루는 하위 장치 (LVM, RAID, LUKS → "sda3" 등)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backing: Vec<String>,
}

impl DiskInfo {
    pub fn has_role(&self, r: DiskRole) -> bool {
        self.roles.contains(&r)
    }
    /// 디스크 전체를 이미징 대상으로 고를 수 없음 (저장 폴더가 이 디스크에 있음). 다른 파티션은 고를 수 있음
    pub fn locked(&self) -> bool {
        self.has_role(DiskRole::Output)
    }
    /// 외부 연결 디스크 (쓰기 방지 장치 권고 대상)
    pub fn external(&self) -> bool {
        self.removable || matches!(self.bus.as_str(), "USB" | "IEEE 1394" | "SD" | "MMC")
    }
    /// "내장" / "외장" / "이동식"
    pub fn kind_label(&self) -> &'static str {
        if self.removable {
            crate::tr!("이동식", "removable")
        } else if self.external() {
            crate::tr!("외장", "external")
        } else {
            crate::tr!("내장", "internal")
        }
    }
}

/// 드라이브 문자 기준 볼륨 정보 (저장 폴더 검사용)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    pub letter: String,
    pub label: String,
    pub fs: String,
    #[serde(with = "u64_str")]
    pub size: u64,
    #[serde(with = "u64_str")]
    pub free: u64,
    pub disk_number: Option<u32>,
    #[serde(with = "u64_str")]
    pub offset: u64,
    pub drive_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcInfo {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
}
