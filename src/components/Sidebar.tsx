import { go, jobCount, neededBytes, openReceive, resultKind, rxActive, useApp, type Screen } from "../store";
import { duration, size } from "../format";
import { Icon } from "./Icon";

const STEPS: { id: Screen; title: string }[] = [
  { id: "case", title: "사건 정보" },
  { id: "select", title: "수집 항목" },
  { id: "run", title: "수집 진행" },
  { id: "done", title: "결과" },
];

export function Sidebar() {
  const s = useApp();
  const inRx = s.mode === "receive";
  const idx = inRx ? -1 : STEPS.findIndex((x) => x.id === s.screen);
  const running = s.starting || (s.started && !s.finished && !s.cancelled);
  const rxBusy = rxActive(s);
  const rxSub = {
    idle: "이 PC가 증거를 받습니다",
    starting: "준비 중…",
    listening: `코드 ${s.rx.code}, 접속 대기`,
    receiving: s.rx.need ? `받는 중 ${Math.min(100, Math.floor((s.rx.total / s.rx.need) * 100))}%` : "받는 중",
    done: "수신 완료",
    stopped: "중지됨",
    failed: "수신 실패",
  }[s.rx.phase];

  const sub: Record<Screen, string> = {
    case: s.form.caseNo.trim() || "입력 중",
    select: s.screen === "case" ? "대상과 옵션 선택" : `대상 ${jobCount(s)}개, ${size(neededBytes(s))}`,
    run: s.finished ? `완료, ${duration((s.summary ? Date.parse(s.summary.finished.slice(0, 19)) - Date.parse(s.summary.started.slice(0, 19)) : 0) / 1000)}` : s.cancelled && s.started ? "중단됨" : s.started ? "진행 중" : "대기",
    done: s.finished && s.summary ? { verified: "해시 검증 일치", unverified: "검증 안 함", badsectors: "읽기 오류 있음", partial: "일부 실패" }[resultKind(s.summary)] : "대기",
  };

  const locked = (id: Screen, i: number) =>
    rxBusy || (id === "run" && !s.started) || (id === "done" && !s.finished) || (running && i < 2);

  return (
    <nav className="nav" aria-label="수집 단계">
      <div className="brand">
        <span className="brand__mark" aria-hidden="true">WS</span>
        <span className="brand__name" translate="no">WS Collector</span>
      </div>
      <ol className="steps">
        {STEPS.map((st, i) => {
          const isLocked = locked(st.id, i);
          const done = i < idx || (st.id === "run" && s.finished);
          return (
            <li key={st.id} className={[!inRx && st.id === s.screen && "is-current", done && "is-done", isLocked && "is-locked"].filter(Boolean).join(" ")}>
              <a
                href={`#${st.id}`}
                aria-current={!inRx && st.id === s.screen ? "step" : undefined}
                aria-disabled={isLocked || undefined}
                tabIndex={isLocked ? -1 : undefined}
                onClick={(e) => {
                  e.preventDefault();
                  if (!isLocked) go(st.id);
                }}
              >
                <span className="steps__dot">
                  <span className="n">{i + 1}</span>
                  <Icon name="check" className="" />
                </span>
                <span className="steps__text">
                  <b>{st.title}</b>
                  <small>{sub[st.id]}</small>
                </span>
              </a>
            </li>
          );
        })}
      </ol>
      <p className="nav__sep">다른 PC에서 받기</p>
      <ul className="steps">
        <li className={[inRx && "is-current", running && "is-locked"].filter(Boolean).join(" ")}>
          <a
            href="#receive"
            aria-current={inRx ? "page" : undefined}
            aria-disabled={running || undefined}
            tabIndex={running ? -1 : undefined}
            onClick={(e) => {
              e.preventDefault();
              if (!running) openReceive();
            }}
          >
            <span className={`steps__dot steps__dot--icon${rxBusy ? " is-live" : ""}`}>
              <Icon name="download" className="" />
            </span>
            <span className="steps__text">
              <b>네트워크 수신</b>
              <small>{rxSub}</small>
            </span>
          </a>
        </li>
      </ul>
      <div className={`nav__status${s.env && !s.env.elevated ? " is-off" : ""}${running ? " is-run" : ""}`}>
        <Icon name="shield" />
        <span>
          <b>{s.env ? (s.env.elevated ? (s.env.os === "linux" ? "root 권한" : "관리자 권한") : "일반 사용자 권한") : "확인 중…"}</b>
          <small>
            {s.env
              ? `환경 점검 ${s.env.checks.filter((c) => c.status === "pass").length}개 통과${s.env.checks.some((c) => c.status !== "pass") ? `, ${s.env.checks.filter((c) => c.status !== "pass").length}개 확인 필요` : ""}`
              : ""}
          </small>
        </span>
      </div>
    </nav>
  );
}
