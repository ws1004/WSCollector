//! OS별 구현. 공통 함수 목록:
//! - `host_info()`, `is_elevated()`, `ADMIN_HINT`
//! - `list_disks()` (역할 미지정), `volume_for_path()`, `disks_for_path()`, `system_disks()`
//! - `open_target()` 이미징 원본 열기
//! - `process_tree()`, `footprint_notes()` 흔적 기록용

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(not(any(windows, target_os = "linux")))]
mod other;
#[cfg(not(any(windows, target_os = "linux")))]
pub use self::other::*;

/// 언어에 따라 바뀌는 고정 안내 문구 (`ADMIN_HINT` 등). `{}`로 출력하거나 `&str`처럼 씀
#[derive(Debug, Clone, Copy)]
pub struct Bilingual {
    pub ko: &'static str,
    pub en: &'static str,
}

impl Bilingual {
    pub fn as_str(&self) -> &'static str {
        crate::tr!(self.ko, self.en)
    }
}

impl std::fmt::Display for Bilingual {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::ops::Deref for Bilingual {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
