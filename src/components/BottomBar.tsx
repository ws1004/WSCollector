import { useRef } from "react";
import { askStopReceive, freeBytes, go, jobCount, resultKind, neededBytes, newCollection, resetReceive, startCollection, startReceive, submitCase, useApp } from "../store";
import { rate, size } from "../format";
import { Icon } from "./Icon";

export function BottomBar() {
  const s = useApp();
  const busy = useRef(false);

  let sum: React.ReactNode = null;
  let actions: React.ReactNode = null;

  if (s.mode === "receive") {
    const r = s.rx;
    const pct = r.need ? Math.min(100, Math.floor((r.total / r.need) * 100)) : 0;
    if (r.phase === "idle" || r.phase === "starting") {
      sum = <span>저장 폴더<b className="mono">{r.outBase.trim() || "미선택"}</b></span>;
      actions = (
        <button type="button" className="btn btn--primary" disabled={r.phase === "starting"} onClick={() => void startReceive()}>
          <Icon name="download" />{r.phase === "starting" ? "준비 중…" : "수신 시작"}
        </button>
      );
    } else if (r.phase === "listening" || r.phase === "receiving") {
      sum =
        r.phase === "listening" ? (
          <span>연결 코드<b className="mono">{r.code}</b></span>
        ) : (
          <>
            <span>받는 중<b>{pct}%</b></span>
            <span>속도<b>{r.verify ? "검증 중" : rate(r.rate)}</b></span>
          </>
        );
      actions = (
        <button type="button" className="btn btn--ghost" onClick={askStopReceive}>
          <Icon name="stop" />수신 중지
        </button>
      );
    } else {
      sum = (
        <span className={r.phase === "done" ? undefined : "is-bad"}>
          <b>{r.phase === "done" ? `수신 완료, 파일 ${r.outcome?.files.length ?? 0}개` : r.phase === "stopped" ? "수신을 중지했습니다" : "수신이 실패했습니다"}</b>
        </span>
      );
      actions = (
        <button type="button" className="btn btn--primary" onClick={resetReceive}>
          <Icon name="refresh" />다음 수신 준비
        </button>
      );
    }
  } else if (s.screen === "case") {
    sum = (
      <>
        <span>사건<b>{s.form.caseNo.trim() || "미입력"}</b></span>
        <span>증거<b>{s.form.evNo.trim() || "미입력"}</b></span>
        <span>수집자<b>{s.form.examiner.trim() || "미입력"}</b></span>
      </>
    );
    actions = (
      <button
        type="button"
        className="btn btn--primary"
        onClick={async () => {
          if (busy.current) return;
          busy.current = true;
          const bad = await submitCase();
          busy.current = false;
          if (bad) document.getElementById(bad)?.focus();
        }}
      >
        다음: 수집 항목
      </button>
    );
  } else if (s.screen === "select") {
    const need = neededBytes(s);
    const free = freeBytes(s);
    const over = free != null && need > free;
    const n = jobCount(s);
    const dis = n === 0 || over || s.disksState !== "ready" || s.starting;
    sum = s.dest === "net" ? (
      <>
        <span>보낼 크기<b>{size(need)}</b></span>
        <span>저장<b className="mono">수신 PC {s.form.netAddr.trim()}</b></span>
      </>
    ) : (
      <div className="cap">
        <div className="cap__text">
          <span>필요한 공간 <b>{size(need)}</b></span>
          <span className={over ? "is-bad" : undefined}>여유 공간 <b>{free != null ? size(free) : "알 수 없음"}</b>{over ? " (부족)" : ""}</span>
        </div>
        <div className="cap__track">
          <div className={`cap__fill${over ? " is-over" : ""}`} style={{ transform: `scaleX(${free ? Math.min(need / free, 1) : 0})` }} />
        </div>
      </div>
    );
    actions = (
      <>
        <button type="button" className="btn btn--ghost" onClick={() => go("case")}>이전</button>
        <button type="button" className="btn btn--primary" disabled={dis} onClick={() => void startCollection()}>
          <Icon name="play" />{s.starting ? "검사하는 중…" : `작업 ${n}개 시작`}
        </button>
      </>
    );
  } else if (s.screen === "run") {
    const left = s.jobs.filter((j) => j.status === "wait" || j.status === "run").length;
    sum = s.finished ? (
      <span>모든 작업을 마쳤습니다</span>
    ) : s.cancelled ? (
      <span className="is-bad"><b>{s.failMessage ? "수집이 실패했습니다" : "수집을 중단했습니다"}</b></span>
    ) : (
      <>
        <span>남은 작업<b>{left}개</b></span>
        <span>{s.runRemote ? "수신 PC 저장 위치" : "저장 위치"}<b className="mono">{s.evidenceDir || (s.runRemote ? `${s.runRemote}에 연결하는 중…` : "준비 중…")}</b></span>
      </>
    );
    actions = s.finished ? (
      <button type="button" className="btn btn--primary" onClick={() => go("done")}>결과 보기</button>
    ) : s.cancelled ? (
      s.summary ? (
        <>
          <button type="button" className="btn btn--ghost" onClick={() => useApp.setState({ finished: true, screen: "done" })}>남은 기록 보기</button>
          <button type="button" className="btn btn--primary" onClick={newCollection}>새 수집 시작</button>
        </>
      ) : (
        <button type="button" className="btn btn--primary" onClick={newCollection}>새 수집 시작</button>
      )
    ) : (
      <button type="button" className="btn btn--ghost" onClick={() => useApp.setState({ cancelOpen: true })}>
        <Icon name="stop" />수집 중단
      </button>
    );
  } else if (s.screen === "done" && s.summary) {
    const kind = resultKind(s.summary);
    const ok = s.summary.jobs.filter((j) => j.result?.verify?.matched).length;
    const label = { verified: "모두 일치", unverified: "검증 안 함", badsectors: `${ok}개 일치, 읽기 오류 있음`, partial: `${ok}개 일치, 일부 실패` }[kind];
    sum = (
      <>
        <span>증거<b>{s.summary.jobs.length}개</b></span>
        <span>해시 검증<b style={{ color: kind === "verified" ? "var(--ok)" : "var(--warn)" }}>{label}</b></span>
      </>
    );
    actions = <button type="button" className="btn btn--ghost" onClick={newCollection}>새 수집 시작</button>;
  }

  return (
    <footer className="bar">
      <div className="bar__sum">{sum}</div>
      <div className="bar__actions">{actions}</div>
    </footer>
  );
}
