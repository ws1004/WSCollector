//! 화면, 로그, 보고서 문구의 언어 (한국어 / 영어).
//!
//! - 기본은 한국어. GUI는 바꾸지 않으므로 늘 한국어
//! - CLI, TUI는 시작할 때 `set_lang`으로 정함 (`--lang`, `WSC_LANG`, 자동 판별)
//! - 문구는 `tr!(한국어, 영어)`로 씀. 고른 쪽만 계산됨
//!
//! ```ignore
//! log(tr!(format!("읽기 완료: {n}"), format!("Read complete: {n}")));
//! let s: &str = tr!("완료", "Done");
//! ```

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ko,
    En,
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(if l == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Ko
    }
}

/// 영어로 출력 중인지
pub fn en() -> bool {
    lang() == Lang::En
}

/// "ko", "en", "korean", "english" 등 → 언어
pub fn parse(s: &str) -> Option<Lang> {
    let s = s.trim().to_ascii_lowercase();
    if s.starts_with("ko") {
        Some(Lang::Ko)
    } else if s.starts_with("en") {
        Some(Lang::En)
    } else {
        None
    }
}

/// 터미널(CLI, TUI) 언어 자동 판별: 한글을 확실히 표시할 수 있을 때만 한국어
/// - Linux 등: 로캘(LC_ALL → LC_CTYPE → LANG)이 ko로 시작하고 UTF-8이며, 리눅스 가상 콘솔(TERM=linux, 한글 글꼴 없음)이 아닐 때
/// - Windows: 사용자 표시 언어가 한국어일 때 (한국어 Windows 콘솔은 한글 표시 가능)
pub fn detect_terminal() -> Lang {
    #[cfg(windows)]
    {
        // SAFETY: 인자 없는 조회 함수
        let id = unsafe { GetUserDefaultUILanguage() };
        if id & 0x3ff == 0x12 {
            Lang::Ko
        } else {
            Lang::En
        }
    }
    #[cfg(not(windows))]
    {
        let loc = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let utf8 = loc.contains("utf-8") || loc.contains("utf8");
        let console = std::env::var("TERM").map(|t| t == "linux").unwrap_or(false);
        if loc.starts_with("ko") && utf8 && !console {
            Lang::Ko
        } else {
            Lang::En
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// 세션 상태값 표시: 보내는 쪽과 받는 쪽의 언어가 다를 수 있어 상태값("완료","일부 실패","중단")은
/// 프로토콜 값으로 한국어 그대로 주고받고, 화면에 보일 때만 바꿈
pub fn status_label(s: &str) -> String {
    if !en() {
        return s.to_string();
    }
    match s {
        "완료" => "Completed",
        "일부 실패" => "Partially failed",
        "중단" => "Cancelled",
        _ if s.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)) => "Finished",
        _ => s,
    }
    .to_string()
}

/// 언어에 따라 둘 중 하나. 고른 쪽 식만 계산됨
#[macro_export]
macro_rules! tr {
    ($ko:expr, $en:expr $(,)?) => {
        if $crate::i18n::en() {
            $en
        } else {
            $ko
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_names() {
        assert_eq!(parse("EN"), Some(Lang::En));
        assert_eq!(parse("ko_KR"), Some(Lang::Ko));
        assert_eq!(parse("fr"), None);
    }
}
