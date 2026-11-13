import { useEffect, useRef, useState } from "react";
import { useApp, type JobRun } from "../store";
import { duration, rate, size } from "../format";
import { Icon } from "../components/Icon";

function weight(j: JobRun, verify: boolean) {
  return Number(j.plan.size) * (verify ? 2 : 1);
}

function jobDone(j: JobRun, verify: boolean): number {
  const total = Number(j.plan.size);
  if (j.status === "done") return weight(j, verify);
  const p = j.progress;
  if (!p) return 0;
  return p.phase === "reading" ? p.done : total + p.done;
}

function Pill({ j }: { j: JobRun }) {
  switch (j.status) {
    case "done":
      return <span className="pill pill--ok"><Icon name="check" />완료</span>;
    case "failed":
      return <span className="pill pill--stop">실패</span>;
    case "cancelled":
      return <span className="pill pill--stop">중단</span>;
    case "skipped":
      return <span className="pill pill--wait">건너뜀</span>;
    case "run":
      return <span className="pill pill--run">{j.progress?.phase === "verifying" ? "검증 중" : "읽는 중"}</span>;
    default:
      return <span className="pill pill--wait">대기</span>;
  }
}

function JobRow({ j, verify }: { j: JobRun; verify: boolean }) {
  const p = j.progress;
  const frac = j.status === "done" ? 1 : p ? p.done / Math.max(p.total, 1) : 0;
  const cls = j.status === "run" ? "is-run" : j.status === "done" ? "is-done" : j.status === "wait" ? "is-wait" : "is-stop";
  const sha = j.outcome?.result?.hashes.sha256 ?? j.outcome?.result?.hashes.sha1 ?? j.outcome?.result?.hashes.md5;
  const bad = p?.badSectors ?? j.outcome?.result?.badSectors ?? 0;
  return (
    <li className={`job ${cls}`}>
      <span className="job__icon"><Icon name={j.plan.fileBase.endsWith("_Memory") ? "mem" : "disk"} className="" /></span>
      <span className="job__title"><b>{j.plan.fileBase.endsWith("_Memory") ? "메모리 수집" : `디스크 이미징: ${j.plan.title}`}</b><small>{j.plan.description}, {size(j.plan.size)}</small></span>
      <span className="job__pct">{j.status === "done" ? <Pill j={j} /> : `${Math.floor(frac * 100)}%`}</span>
      <div className="progress" role="progressbar" aria-label={`${j.plan.title} 진행률`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.floor(frac * 100)}>
        <div className="progress__fill" style={{ transform: `scaleX(${frac})` }} />
      </div>
      <div className="job__meta">
        {j.status !== "done" && <Pill j={j} />}
        {j.status === "run" && p && <span>속도<b> {rate(p.rateBps)}</b></span>}
        {j.status === "run" && p && <span>남은 시간<b> {duration(p.etaSecs + (p.phase === "reading" && verify ? p.total / Math.max(p.rateBps, 1) : 0))}</b></span>}
        {j.status === "done" && sha && <span>해시<b className="mono"> {sha.slice(0, 20)}…</b></span>}
        {bad > 0 && <span>읽기 오류<b> {bad}섹터</b></span>}
        {j.outcome?.error && <span>오류<b> {j.outcome.error}</b></span>}
      </div>
    </li>
  );
}

export function RunScreen() {
  const s = useApp();
  const [now, setNow] = useState(Date.now());
  const logRef = useRef<HTMLOListElement>(null);
  const stick = useRef(true);
  const running = s.started && !s.finished && !s.cancelled;

  useEffect(() => {
    if (!running) return;
    const h = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(h);
  }, [running]);

  useEffect(() => {
    const el = logRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [s.logs.length]);

  const total = s.jobs.reduce((a, j) => a + weight(j, s.verify), 0);
  const done = s.jobs.reduce((a, j) => a + jobDone(j, s.verify), 0);
  const pct = s.finished ? 100 : total ? Math.floor((done / total) * 100) : 0;
  const cur = s.jobs.findIndex((j) => j.status === "run");
  const endAt = s.summary ? Date.parse(s.summary.finished.slice(0, 19)) - Date.parse(s.summary.started.slice(0, 19)) : null;
  const elapsed = endAt != null && !running ? endAt / 1000 : (now - s.startedAt) / 1000;

  return (
    <section className="screen" aria-labelledby="h-run">
      <header className="page-head page-head--row">
        <div>
          <h1 id="h-run" tabIndex={-1}>수집 진행</h1>
          <p>
            {s.finished
              ? "모든 작업을 마쳤습니다. 결과 화면에서 해시를 확인하세요."
              : s.cancelled
                ? s.failMessage ?? "수집을 중단했습니다. 지금까지 읽은 파일은 증거로 쓸 수 없습니다."
                : s.runRemote
                  ? `수신 PC(${s.runRemote})로 보내는 중입니다. 창을 닫거나 네트워크 연결을 끊지 마세요.`
                  : "창을 닫거나 USB를 뽑지 마세요. 증거 파일이 손상될 수 있습니다."}
          </p>
        </div>
        <div className="elapsed"><small>경과 시간</small><b className="mono">{duration(Math.max(0, elapsed))}</b></div>
      </header>

      <section className={`panel overall${s.finished ? " is-done" : ""}${s.cancelled ? " is-stop" : ""}`} aria-labelledby="h-overall">
        <div className="overall__top">
          <h2 id="h-overall">전체 진행률</h2>
          <b className="overall__pct">{pct}%</b>
        </div>
        <div className="progress progress--lg" role="progressbar" aria-labelledby="h-overall" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
          <div className="progress__fill" style={{ transform: `scaleX(${pct / 100})` }} />
        </div>
        <p className="overall__sub">
          {s.finished
            ? `작업 ${s.jobs.length}개 모두 끝남`
            : s.cancelled
              ? "수집이 멈췄습니다"
              : cur >= 0
                ? `작업 ${s.jobs.length}개 중 ${cur + 1}번째 진행 중`
                : "준비 중…"}
          {s.evidenceDir && <span className="mono overall__path">{s.runRemote ? `수신 PC ${s.runRemote}: ` : ""}{s.evidenceDir}</span>}
        </p>
      </section>

      <div className="run-grid">
        <section className="panel" aria-labelledby="h-jobs">
          <header className="panel__head"><h2 id="h-jobs">작업</h2></header>
          <ol className="jobs">
            {s.jobs.map((j, i) => <JobRow key={i} j={j} verify={s.verify} />)}
            {s.jobs.length === 0 && (
              <li className="job job--placeholder is-wait">
                <span className="job__title"><b>
                  {s.cancelled
                    ? s.runRemote
                      ? `수신 PC(${s.runRemote})에 연결하지 못했습니다`
                      : "저장 폴더를 준비하지 못했습니다"
                    : s.runRemote
                      ? `수신 PC(${s.runRemote})에 연결하는 중…`
                      : "저장 폴더를 준비하는 중…"}
                </b></span>
              </li>
            )}
          </ol>
        </section>
        <section className="panel" aria-labelledby="h-log">
          <header className="panel__head"><h2 id="h-log">수집 로그</h2><span className="count mono">collection.log</span></header>
          <ol
            className="log"
            ref={logRef}
            tabIndex={0}
            aria-label="수집 로그"
            onScroll={(e) => {
              const el = e.currentTarget;
              stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
            }}
          >
            {s.logs.map((l, i) => (
              <li key={i} className={l.level === "info" ? undefined : "is-warn"}>
                <time>{l.time.slice(11, 19)}</time>
                <span>{l.message}</span>
              </li>
            ))}
          </ol>
        </section>
      </div>
    </section>
  );
}
