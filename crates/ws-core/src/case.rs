//! 사건 정보와 저장 경로 규칙.
//!
//! 저장 경로 = `저장 폴더 / 사건 번호 / 증거 번호`
//! 사건 번호와 증거 번호 폴더는 수집을 시작할 때 자동으로 만듭니다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 사건 정보. GUI 사건 정보 화면, CLI 인자가 같은 구조로 채웁니다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CaseInfo {
    pub case_number: String,
    pub evidence_number: String,
    pub examiner: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CaseError {
    Empty(&'static str),
    InvalidChars(&'static str),
    Reserved(&'static str),
    TooLong(&'static str, usize),
    Bidi(&'static str),
}

impl std::fmt::Display for CaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaseError::Empty(l) => f.write_str(&crate::tr!(format!("{l} 항목이 비어 있습니다"), format!("{l} is required"))),
            CaseError::InvalidChars(l) => f.write_str(&crate::tr!(
                format!("{l}에 폴더 이름으로 쓸 수 없는 문자가 있습니다 (\\ / : * ? \" < > |)"),
                format!("{l} contains characters not allowed in folder names (\\ / : * ? \" < > |)")
            )),
            CaseError::Reserved(l) => f.write_str(&crate::tr!(
                format!("{l}은(는) 폴더 이름으로 쓸 수 없습니다"),
                format!("{l} cannot be used as a folder name")
            )),
            CaseError::TooLong(l, n) => f.write_str(&crate::tr!(
                format!("{l}이(가) 너무 깁니다 (최대 {n}자)"),
                format!("{l} is too long (max {n} characters)")
            )),
            CaseError::Bidi(l) => f.write_str(&crate::tr!(
                format!("{l}에 보이지 않는 글자 방향 제어 문자가 있습니다 (복사, 붙여넣기로 들어왔을 수 있음)"),
                format!("{l} contains invisible text-direction control characters (possibly from copy and paste)")
            )),
        }
    }
}

impl std::error::Error for CaseError {}

const MAX_SEGMENT_LEN: usize = 100;

/// Windows와 다른 OS 모두에서 안전한 폴더 이름인지 검사합니다.
pub fn validate_segment(label: &'static str, value: &str) -> Result<(), CaseError> {
    let v = value.trim();
    if v.is_empty() {
        return Err(CaseError::Empty(label));
    }
    if v.chars().count() > MAX_SEGMENT_LEN {
        return Err(CaseError::TooLong(label, MAX_SEGMENT_LEN));
    }
    if v.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        return Err(CaseError::InvalidChars(label));
    }
    if v.chars().any(crate::util::is_bidi) {
        return Err(CaseError::Bidi(label));
    }
    // 점으로 끝나는 이름, ".", ".." 은 Windows에서 문제를 일으킵니다
    if v == "." || v == ".." || v.ends_with('.') {
        return Err(CaseError::Reserved(label));
    }
    // Windows 예약 장치 이름 (CON, PRN, AUX, NUL, COM1~9, LPT1~9)
    let stem = v.split('.').next().unwrap_or(v).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        return Err(CaseError::Reserved(label));
    }
    Ok(())
}

impl CaseInfo {
    /// 필수 항목과 폴더 이름 규칙을 검사합니다.
    pub fn validate(&self) -> Result<(), CaseError> {
        validate_segment(crate::tr!("사건 번호", "Case number"), &self.case_number)?;
        validate_segment(crate::tr!("증거 번호", "Evidence number"), &self.evidence_number)?;
        if self.examiner.trim().is_empty() {
            return Err(CaseError::Empty(crate::tr!("수집자", "Examiner")));
        }
        // 이름 칸에는 보이지 않는 방향, 서식 문자를 받지 않음 (메모는 로그, 보고서에서 공백으로 바꿈)
        for (label, v) in [(crate::tr!("수집자", "Examiner"), &self.examiner), (crate::tr!("소속", "Organization"), &self.organization)] {
            if v.chars().any(crate::util::is_bidi) {
                return Err(CaseError::Bidi(label));
            }
        }
        Ok(())
    }

    /// `저장 폴더 / 사건 번호 / 증거 번호`
    pub fn evidence_dir(&self, base: &Path) -> PathBuf {
        base.join(self.case_number.trim()).join(self.evidence_number.trim())
    }

    /// 증거 파일 이름 접두어: `증거번호_호스트명_YYYYMMDD`
    pub fn file_prefix(&self, hostname: &str, date_yyyymmdd: &str) -> String {
        format!("{}_{}_{}", self.evidence_number.trim(), hostname, date_yyyymmdd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CaseInfo {
        CaseInfo {
            case_number: "2026-형제-01187".into(),
            evidence_number: "EV-003".into(),
            examiner: "홍길동".into(),
            organization: String::new(),
            notes: String::new(),
        }
    }

    #[test]
    fn builds_evidence_dir() {
        let dir = sample().evidence_dir(Path::new("E:/Cases"));
        assert_eq!(dir, Path::new("E:/Cases").join("2026-형제-01187").join("EV-003"));
    }

    #[test]
    fn rejects_bad_segments() {
        assert_eq!(validate_segment("사건 번호", "a/b"), Err(CaseError::InvalidChars("사건 번호")));
        assert_eq!(validate_segment("사건 번호", "  "), Err(CaseError::Empty("사건 번호")));
        assert_eq!(validate_segment("증거 번호", "CON"), Err(CaseError::Reserved("증거 번호")));
        assert_eq!(validate_segment("증거 번호", "com3.txt"), Err(CaseError::Reserved("증거 번호")));
        assert_eq!(validate_segment("증거 번호", "EV."), Err(CaseError::Reserved("증거 번호")));
        assert!(validate_segment("증거 번호", "COM0").is_ok());
        assert!(validate_segment("증거 번호", "EV-003").is_ok());
    }

    #[test]
    fn requires_examiner() {
        let mut c = sample();
        c.examiner = " ".into();
        assert_eq!(c.validate(), Err(CaseError::Empty("수집자")));
    }

    #[test]
    fn file_prefix_format() {
        assert_eq!(sample().file_prefix("DESKTOP-7Q2LM4K", "20261105"), "EV-003_DESKTOP-7Q2LM4K_20261105");
    }
}
