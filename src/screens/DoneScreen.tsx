import { resultKind, useApp, toast } from "../store";
import { api } from "../api";
import { ALG_LABEL, duration, size } from "../format";
import { Icon } from "../components/Icon";
import type { HashAlg } from "../types";

export function DoneScreen() {
  const sm = useApp((s) => s.summary);
  const remote = useApp((s) => s.runRemote);
  if (!sm) return null;
  const sep = sm.evidenceDir.includes("\\") ? "\\" : "/";
  const reveal = (p: string) => api.reveal(p).catch((e) => toast(String(e)));
  const kind = resultKind(sm);
  const ok = kind === "verified";
  const bad = sm.jobs.reduce((a, j) => a + (j.result?.badSectors ?? 0), 0);
  return (
    <section className="screen" aria-labelledby="h-done">
      <header className="page-head page-head--row">
        <div>
          <h1 id="h-done" tabIndex={-1}>결과</h1>
          <p>아래 해시 값과 흔적 기록이 보고서에 그대로 들어갑니다.</p>
        </div>
        {!remote && (
          <div className="actions">
            <button type="button" className="btn btn--ghost" onClick={() => reveal(sm.evidenceDir)}>
              <Icon name="folder" />저장 폴더 열기
            </button>
            <button type="button" className="btn btn--primary" onClick={() => reveal(`${sm.evidenceDir}${sep}${sm.reportFile}`)}>
              <Icon name="file" />보고서 파일 보기
            </button>
          </div>
        )}
      </header>

      {remote && (
        <div className="dest dest--remote">
          <Icon name="send" />
          <span>
            <b>증거 파일과 보고서는 수신 PC({remote})에 저장했습니다</b>
            <small className="mono">{sm.evidenceDir}{sep}{sm.reportFile}</small>
          </span>
        </div>
      )}

      <div className={`infobar ${ok ? "infobar--ok" : "infobar--warn"}`}>
        <Icon name={ok ? "check-c" : "alert"} />
        <p>
          {kind === "verified" && <><b>수집을 마쳤습니다.</b> 모든 증거 파일을 다시 읽어 해시가 일치하는 것을 확인했습니다.</>}
          {kind === "unverified" && <><b>수집을 마쳤습니다.</b> 이미징 후 검증은 하지 않았습니다. 해시는 원본에서 읽은 데이터 기준입니다.</>}
          {kind === "badsectors" && <><b>수집을 마쳤지만 읽기 오류가 있었습니다.</b> 섹터 {bad}개를 0으로 채웠고, 위치는 보고서와 로그에 기록했습니다.</>}
          {kind === "partial" && <><b>일부 작업이 끝나지 않았습니다.</b> 아래에서 실패하거나 중단된 항목을 확인하세요.</>}
        </p>
      </div>

      <section aria-labelledby="h-files">
        <h2 className="section-title" id="h-files">증거 파일</h2>
        <div className="results">
          {sm.jobs.map((j) => {
            const r = j.result;
            const v = r?.verify;
            return (
              <article key={j.index} className="panel result">
                <div className="result__head">
                  <span className="result__icon"><Icon name={j.title === "메모리" ? "mem" : "disk"} className="" /></span>
                  <span className="result__name">
                    <b>{j.title}</b>
                    <small>
                      {j.description}
                      {r && `, ${size(r.totalBytes)}`}
                      {r && r.format === "e01" && r.totalBytes > 0 && ` → E01 ${size(r.storedBytes)} (${((r.storedBytes * 100) / r.totalBytes).toFixed(1)}%)`}
                      {r && `, 파일 ${r.files.length}개, ${duration(r.readSecs)}`}
                      {r && r.badSectors > 0 && `, 읽기 오류 ${r.badSectors}섹터 0으로 채움`}
                    </small>
                  </span>
                  {v ? (
                    v.matched ? <span className="pill pill--ok"><Icon name="check" />검증 일치</span> : <span className="pill pill--stop">{v.error ? "검증 실패" : "검증 불일치"}</span>
                  ) : j.status === "done" ? (
                    <span className="pill pill--wait">검증 안 함</span>
                  ) : (
                    <span className="pill pill--stop">{j.status === "failed" ? "실패" : j.status === "cancelled" ? "중단" : "건너뜀"}</span>
                  )}
                </div>
                {j.error && <p className="result__error">{j.error}</p>}
                {r?.verify?.error && <p className="result__error">검증을 끝내지 못했습니다: {r.verify.error}</p>}
                {r && (
                  <dl className="hashes">
                    {(Object.keys(r.hashes) as HashAlg[]).map((k) => (
                      <div key={k}>
                        <dt>{ALG_LABEL[k]}</dt>
                        <dd>{r.hashes[k]}</dd>
                        <button
                          type="button"
                          className="icon-btn"
                          aria-label={`${ALG_LABEL[k]} 복사`}
                          onClick={() => navigator.clipboard.writeText(r.hashes[k] ?? "").then(() => toast(`${ALG_LABEL[k]} 해시를 복사했습니다.`))}
                        >
                          <Icon name="copy" className="" />
                        </button>
                      </div>
                    ))}
                  </dl>
                )}
                {r && (
                  <p className="result__files mono">
                    {r.files.length <= 3 ? r.files.map((f) => f.name).join(", ") : `${r.files[0].name} … ${r.files[r.files.length - 1].name}`}
                  </p>
                )}
              </article>
            );
          })}
        </div>
      </section>

      <section className="panel" aria-labelledby="h-fp">
        <header className="panel__head">
          <h2 id="h-fp">대상 시스템에 남긴 흔적</h2>
          <span className="count">분석할 때 WS Collector가 만든 기록을 구분하는 데 씁니다</span>
        </header>
        <table className="table">
          <thead><tr><th scope="col">항목</th><th scope="col">내용</th><th scope="col">원인</th></tr></thead>
          <tbody>
            {sm.footprint.outputOnSystemDisk ? (
              <tr><td>대상 디스크 쓰기</td><td>있음: 증거 파일과 로그를 이 PC의 시스템 디스크에 저장</td><td>저장 위치로 시스템 디스크를 고름 (오프라인 이미징이면 해당 없음)</td></tr>
            ) : (
              <tr><td>대상 디스크 쓰기</td><td>없음 (설계 기준, 측정값 아님)</td><td>{remote ? "증거 파일, 로그를 모두 네트워크로 수신 PC에 저장" : "출력, 로그, 임시 파일을 모두 외부 매체에 기록"}</td></tr>
            )}
            <tr>
              <td>실행한 프로세스</td>
              <td className="mono">{sm.footprint.processes.map((p) => `${p.name} (PID ${p.pid})`).join(", ")}</td>
              <td>프로그램 실행과 화면 표시</td>
            </tr>
            <tr><td>WS Collector 파일</td><td className="mono">SHA-256 {sm.footprint.toolSha256}</td><td>도구 무결성 확인용 (v{sm.toolVersion})</td></tr>
            <tr><td>도구 매체</td><td>{sm.footprint.toolMedia}</td><td>USB 연결 기록이 남음</td></tr>
            {remote && <tr><td>네트워크 연결</td><td className="mono">이 PC → {remote}</td><td>방화벽, EDR 로그에 연결 기록이 남을 수 있음</td></tr>}
            {sm.footprint.notes
              .filter((n) => n.includes("가 남기는 기록:"))
              .map((n) => {
                const [label, ...rest] = n.split(": ");
                return (
                  <tr key={n}><td>{label}</td><td>{rest.join(": ")}</td><td>운영체제 기본 동작 (피할 수 없음)</td></tr>
                );
              })}
          </tbody>
        </table>
      </section>
    </section>
  );
}
