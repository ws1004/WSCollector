//! 수집 보고서 (HTML 한 파일, 외부 자원 없음). 브라우저로 열어 인쇄하거나 PDF로 저장할 수 있습니다.

use crate::collect::{CollectSummary, JobStatus, SessionStatus};
use crate::logbook::{LogEntry, LogLevel};
use crate::tr;
use crate::util::{format_bytes, format_duration};

fn esc(s: &str) -> String {
    crate::util::clean_text(s).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn render(s: &CollectSummary, log: &[LogEntry]) -> String {
    let mut h = String::new();
    let status = match s.status {
        SessionStatus::Completed => (tr!("완료", "Completed"), "ok"),
        SessionStatus::Partial => (tr!("일부 실패", "Partially failed"), "warn"),
        SessionStatus::Cancelled => (tr!("중단", "Cancelled"), "bad"),
    };
    h.push_str(&format!(
        r#"<!DOCTYPE html><html lang="{}"><head><meta charset="utf-8"><title>{} {}</title><style>
body{{font-family:"Segoe UI","Malgun Gothic",sans-serif;color:#1a1a1a;margin:32px auto;max-width:960px;padding:0 24px;line-height:1.5}}
h1{{font-size:24px;margin:0 0 4px}}h2{{font-size:16px;margin:28px 0 8px;padding-bottom:6px;border-bottom:1px solid #ddd}}
table{{width:100%;border-collapse:collapse;font-size:13px}}th, td{{text-align:left;padding:6px 8px;border-bottom:1px solid #eee;vertical-align:top}}
th{{width:180px;color:#555;font-weight:600}}.mono{{font-family:Consolas,"Cascadia Mono",monospace;font-size:12px;word-break:break-all}}
.pill{{display:inline-block;padding:1px 8px;border-radius:999px;font-size:12px;font-weight:600}}.ok{{background:#dff6dd;color:#0f7b0f}}.warn{{background:#fff4ce;color:#9d5d00}}.bad{{background:#fde7e9;color:#c42b1c}}
.muted{{color:#666;font-size:13px}}pre{{white-space:pre-wrap;font-family:Consolas, monospace;font-size:11.5px;background:#f6f6f6;padding:12px;border-radius:6px}}
@media print{{body{{margin:0}}}}</style></head><body>"#,
        tr!("ko", "en"),
        tr!("수집 보고서", "Collection Report"),
        esc(&s.file_prefix)
    ));
    h.push_str(&format!(
        "<h1>{}</h1><p class=\"muted\">WS Collector v{} ({}) , <span class=\"pill {}\">{}</span></p>",
        tr!("디지털 증거 수집 보고서", "Digital Evidence Collection Report"),
        esc(&s.tool_version),
        esc(&s.interface),
        status.1,
        status.0
    ));

    let row = |k: &str, v: &str| format!("<tr><th>{}</th><td>{}</td></tr>", esc(k), v);
    h.push_str(&format!("<h2>{}</h2><table>", tr!("사건 정보", "Case Information")));
    h.push_str(&row(tr!("사건 번호", "Case number"), &esc(&s.case.case_number)));
    h.push_str(&row(tr!("증거 번호", "Evidence number"), &esc(&s.case.evidence_number)));
    h.push_str(&row(tr!("수집자", "Examiner"), &esc(&s.case.examiner)));
    if !s.case.organization.is_empty() {
        h.push_str(&row(tr!("소속", "Organization"), &esc(&s.case.organization)));
    }
    if !s.case.notes.is_empty() {
        h.push_str(&row(tr!("현장 메모", "Notes"), &esc(&s.case.notes)));
    }
    h.push_str(&row(tr!("수집 시작", "Started"), &esc(&s.started)));
    h.push_str(&row(tr!("수집 종료", "Finished"), &esc(&s.finished)));
    h.push_str(&row(tr!("저장 폴더", "Evidence folder"), &format!("<span class=\"mono\">{}</span>", esc(&s.evidence_dir.display().to_string()))));
    h.push_str("</table>");

    let host = &s.host;
    h.push_str(&format!("<h2>{}</h2><table>", tr!("대상 시스템", "Target System")));
    h.push_str(&row(tr!("호스트 이름", "Hostname"), &esc(&host.hostname)));
    h.push_str(&row(tr!("운영체제", "Operating system"), &esc(&format!("{} {} ({}, {})", host.os_name, host.os_version, host.build_label(), host.arch))));
    h.push_str(&row(tr!("프로세서", "Processor"), &esc(&host.cpu)));
    h.push_str(&row(tr!("메모리", "Memory"), &format_bytes(host.ram_bytes)));
    h.push_str(&row(tr!("제조사, 모델", "Manufacturer / model"), &esc(&format!("{} {}", host.manufacturer, host.model))));
    h.push_str(&row(tr!("시스템 시각", "System time"), &esc(&host.local_time)));
    h.push_str(&row(tr!("시간대", "Time zone"), &esc(&host.timezone)));
    h.push_str(&row(tr!("부팅 시각", "Boot time"), &esc(&host.boot_time)));
    h.push_str("</table>");

    h.push_str(&format!("<h2>{}</h2>", tr!("수집 결과", "Collection Results")));
    for j in &s.jobs {
        let (label, cls) = match j.status {
            JobStatus::Done => (tr!("완료", "Completed"), "ok"),
            JobStatus::Failed => (tr!("실패", "Failed"), "bad"),
            JobStatus::Cancelled => (tr!("중단", "Cancelled"), "bad"),
            JobStatus::Skipped => (tr!("건너뜀", "Skipped"), "warn"),
        };
        h.push_str(&format!(
            "<h3 style=\"font-size:14px;margin:18px 0 6px\">{} <span class=\"pill {}\">{}</span></h3><p class=\"muted\">{}</p><table>",
            esc(&j.title),
            cls,
            label,
            esc(&j.description)
        ));
        if let Some(e) = &j.error {
            h.push_str(&row(tr!("오류", "Error"), &esc(e)));
        }
        if let Some(r) = &j.result {
            h.push_str(&row(
                tr!("수집 크기", "Acquired size"),
                &tr!(
                    format!("{} ({} 바이트, 섹터 {}B)", format_bytes(r.total_bytes), r.total_bytes, r.sector_size),
                    format!("{} ({} bytes, {}-byte sectors)", format_bytes(r.total_bytes), r.total_bytes, r.sector_size)
                ),
            ));
            h.push_str(&row(tr!("소요 시간", "Duration"), &format_duration(r.read_secs as u64)));
            h.push_str(&row(
                tr!("저장 형식", "Image format"),
                &match r.format {
                    crate::imaging::ImageFormat::E01 if r.total_bytes > 0 => tr!(
                        format!(
                            "E01 (EnCase 6, 압축 후 {} = 원본의 {:.1}%, 파일 안에 MD5,SHA-1 기록)",
                            format_bytes(r.stored_bytes),
                            r.stored_bytes as f64 * 100.0 / r.total_bytes as f64
                        ),
                        format!(
                            "E01 (EnCase 6, {} after compression = {:.1}% of source, MD5/SHA-1 stored in file)",
                            format_bytes(r.stored_bytes),
                            r.stored_bytes as f64 * 100.0 / r.total_bytes as f64
                        )
                    ),
                    crate::imaging::ImageFormat::E01 => "E01 (EnCase 6)".into(),
                    crate::imaging::ImageFormat::Raw => "RAW".into(),
                },
            ));
            h.push_str(&row(
                tr!("파일", "Files"),
                &format!(
                    "<span class=\"mono\">{}</span>",
                    r.files.iter().map(|f| format!("{} ({})", esc(&f.name), format_bytes(f.size))).collect::<Vec<_>>().join("<br>")
                ),
            ));
            for (alg, hash) in &r.hashes {
                h.push_str(&row(alg.label(), &format!("<span class=\"mono\">{}</span>", esc(hash))));
            }
            match &r.verify {
                Some(v) if v.matched => h.push_str(&row(
                tr!("검증", "Verification"),
                if r.format == crate::imaging::ImageFormat::E01 {
                    tr!(
                        "<span class=\"pill ok\">일치</span> 저장한 E01을 다시 읽어 풀고, 원본, E01에 적힌 해시와 같음을 확인함",
                        "<span class=\"pill ok\">Matched</span> Re-read and decompressed the saved E01; hashes match the source and the hashes stored in the E01"
                    )
                } else {
                    tr!(
                        "<span class=\"pill ok\">일치</span> 저장 파일을 다시 읽어 같은 해시를 확인함",
                        "<span class=\"pill ok\">Matched</span> Re-read the saved files; hashes match"
                    )
                },
            )),
                Some(_) => h.push_str(&row(tr!("검증", "Verification"), tr!("<span class=\"pill bad\">불일치</span>", "<span class=\"pill bad\">Mismatch</span>"))),
                None => h.push_str(&row(tr!("검증", "Verification"), tr!("하지 않음", "Not performed"))),
            }
            if r.bad_sectors > 0 {
                let ranges = r
                    .bad_ranges
                    .iter()
                    .take(100)
                    .map(|b| tr!(format!("섹터 {}부터 {}개", b.start_sector, b.count), format!("{} sector(s) from sector {}", b.count, b.start_sector)))
                    .collect::<Vec<_>>()
                    .join("<br>");
                h.push_str(&row(
                    tr!("읽기 오류", "Read errors"),
                    &tr!(
                        format!("{}개 섹터를 0으로 채움<br><span class=\"mono\">{}</span>", r.bad_sectors, ranges),
                        format!("{} bad sector(s) zero-filled<br><span class=\"mono\">{}</span>", r.bad_sectors, ranges)
                    ),
                ));
            }
        }
        h.push_str("</table>");
    }

    let fp = &s.footprint;
    h.push_str(&format!("<h2>{}</h2><table>", tr!("대상 시스템에 남긴 흔적", "Footprint on Target System")));
    h.push_str(&row(tr!("도구 실행 파일", "Tool executable"), &format!("<span class=\"mono\">{}<br>SHA-256 {}</span>", esc(&fp.tool_path), esc(&fp.tool_sha256))));
    h.push_str(&row(tr!("도구 매체", "Tool media"), &esc(&fp.tool_media)));
    h.push_str(&row(tr!("저장 매체", "Output media"), &esc(&fp.output_media)));
    h.push_str(&row(
        tr!("실행한 프로세스", "Processes run"),
        &format!("<span class=\"mono\">{}</span>", fp.processes.iter().map(|p| format!("{} (PID {})", esc(&p.name), p.pid)).collect::<Vec<_>>().join("<br>")),
    ));
    for n in &fp.notes {
        h.push_str(&row(tr!("참고", "Note"), &esc(n)));
    }
    h.push_str("</table>");

    h.push_str(&format!("<h2>{}</h2><pre>", tr!("수집 로그", "Collection Log")));
    for e in log {
        let lv = match e.level {
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
        };
        h.push_str(&esc(&format!("{} [{}] {}\n", e.time, lv, e.message)));
    }
    h.push_str("</pre></body></html>");
    h
}
