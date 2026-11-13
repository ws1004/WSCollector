//! 표시용 포맷 함수와 시간 도우미.

use chrono::{DateTime, Local};

const KIB: f64 = 1024.0;

/// 보이지 않는 서식 문자: 글자 방향 제어(파일 이름, 사건 정보에서 "evidence\u{202E}fdp.exe"처럼
/// 보이는 이름과 실제 이름을 다르게 만듦), 폭 없는 공백, 결합 문자, 줄, 문단 구분자
pub fn is_bidi(c: char) -> bool {
    // U+200C, U+200D(ZWNJ, ZWJ)는 페르시아어, 인도계 문자, 이모지에 필요한 정상 문자라 뺌
    matches!(c, '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// 로그, 보고서에 넣는 자유 텍스트 정리: 줄바꿈, 제어 문자, 보이지 않는 서식 문자를 공백으로
/// (메모에 줄바꿈을 넣어 가짜 로그 줄을 만들거나 방향 문자로 내용을 뒤집어 보이게 하는 것을 막음)
pub fn clean_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || is_bidi(c) { ' ' } else { c }).collect()
}

/// 사람이 읽기 쉬운 크기 (이진 단위, Windows 탐색기와 같은 기준)
pub fn format_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= KIB.powi(4) {
        format!("{:.2} TB", b / KIB.powi(4))
    } else if b >= KIB.powi(3) {
        format!("{:.2} GB", b / KIB.powi(3))
    } else if b >= KIB.powi(2) {
        format!("{:.0} MB", b / KIB.powi(2))
    } else if b >= KIB {
        format!("{:.0} KB", b / KIB)
    } else {
        format!("{} B", b as u64)
    }
}

/// 초 → HH:MM:SS
pub fn format_duration(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

/// 초당 바이트 → "2.05 GB/s"
pub fn format_rate(bps: f64) -> String {
    if bps >= KIB.powi(3) {
        format!("{:.2} GB/s", bps / KIB.powi(3))
    } else if bps >= KIB.powi(2) {
        format!("{:.1} MB/s", bps / KIB.powi(2))
    } else {
        format!("{:.0} KB/s", bps / KIB)
    }
}

/// 보고서, 로그용 현지 시각 (시간대 포함)
pub fn now_local() -> DateTime<Local> {
    Local::now()
}

pub fn fmt_ts(t: &DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S %:z").to_string()
}

/// "2G", "4096M", "0", "1.5G" → 바이트. 0은 분할 안 함.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_uppercase();
    let s = s.strip_suffix('B').unwrap_or(&s);
    let (num, mul) = match s.chars().last()? {
        'K' => (&s[..s.len() - 1], 1u64 << 10),
        'M' => (&s[..s.len() - 1], 1u64 << 20),
        'G' => (&s[..s.len() - 1], 1u64 << 30),
        'T' => (&s[..s.len() - 1], 1u64 << 40),
        _ => (s, 1u64),
    };
    let v: f64 = num.trim().parse().ok()?;
    if v < 0.0 || !v.is_finite() {
        return None;
    }
    Some((v * mul as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_bytes(1000204886016), "931.51 GB");
        assert_eq!(format_bytes(2000398934016), "1.82 TB");
        assert_eq!(format_bytes(104857600), "100 MB");
        assert_eq!(parse_size("2G"), Some(2 << 30));
        assert_eq!(parse_size("4096m"), Some(4096 << 20));
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size("x"), None);
        assert_eq!(format_duration(3725), "01:02:05");
    }
}
