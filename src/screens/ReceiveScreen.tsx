// 네트워크 수신: 이 PC가 분석 PC가 되어 다른 PC(대상 시스템)가 보내는 증거를 저장합니다.
import { useEffect, useRef } from "react";
import { api } from "../api";
import { checkRxOutput, outExample, pickRxFolder, sepOf, setRxField, toast, useApp, type RxFile } from "../store";
import { rate, size } from "../format";
import { Icon } from "../components/Icon";

const copy = (text: string, what: string) =>
  navigator.clipboard.writeText(text).then(
    () => toast(`${what}을(를) 복사했습니다.`),
    () => toast("복사하지 못했습니다."),
  );

/** 보내는 쪽 입력용 주소: 기본 포트면 IP만 */
function sendAddr(addr: string, port: string) {
  return port === "7700" ? addr.replace(/:7700$/, "") : addr;
}

function HowTo({ addr, code }: { addr: string; code: string }) {
  const a = addr || "<이 PC 주소>";
  const cmd = `sudo ./WSCollector-CLI-linux-x86_64 image -s /dev/sda --send ${a} --case <사건번호> --evidence <증거번호> --examiner <수집자>`;
  return (
    <section className="panel" aria-labelledby="h-howto">
      <header className="panel__head"><h2 id="h-howto">보내는 쪽에서 할 일</h2></header>
      <ol className="howto">
        <li>
          <b>Windows 대상</b>
          <span>
            WS Collector를 실행하고, 사건 정보 화면의 저장 위치에서 <em>네트워크 수신 PC</em>를 고른 뒤
            주소 <code className="mono">{a}</code>와 연결 코드{code ? <> <code className="mono">{code}</code></> : ""}를 입력합니다.
          </span>
        </li>
        <li>
          <b>Linux 서버</b>
          <span>아래 명령을 실행하고, 연결 코드를 물어보면 입력합니다. 대상 이름은 <code className="mono">list</code> 명령으로 확인합니다.</span>
          <div className="cmd">
            <code className="mono">{cmd}</code>
            <button type="button" className="icon-btn" aria-label="명령 복사" onClick={() => void copy(cmd, "명령")}>
              <Icon name="copy" className="" />
            </button>
          </div>
        </li>
      </ol>
    </section>
  );
}

function Notes() {
  return (
    <section className="panel" aria-labelledby="h-rx-notes">
      <header className="panel__head"><h2 id="h-rx-notes">알아둘 점</h2></header>
      <ul className="checks">
        <li className="check is-pass">
          <Icon name="shield" className="check__ic" />
          <span><b>암호화 연결</b><small>연결 코드로 키를 정하고 모든 데이터를 암호화합니다. 코드를 모르는 쪽은 연결할 수 없습니다.</small></span>
        </li>
        <li className="check is-warn">
          <Icon name="alert" className="check__ic" />
          {useApp.getState().env?.os === "linux" ? (
            <span><b>방화벽</b><small>보내는 쪽이 연결하지 못하면 이 포트로 들어오는 연결을 허용하세요. 예: sudo ufw allow 7700/tcp 또는 sudo firewall-cmd --add-port=7700/tcp</small></span>
          ) : (
            <span><b>Windows 방화벽</b><small>처음 수신할 때 방화벽 허용 창이 뜨면 허용하세요. 보내는 쪽이 연결하지 못하면 이 포트로 들어오는 연결을 허용해야 합니다.</small></span>
          )}
        </li>
        <li className="check is-pass">
          <Icon name="info" className="check__ic" />
          <span><b>증거 하나씩</b><small>보내는 쪽 세션 하나를 받으면 수신이 끝납니다. 다음 증거는 다시 수신을 시작하세요.</small></span>
        </li>
      </ul>
    </section>
  );
}

function Setup() {
  const r = useApp((s) => s.rx);
  const oc = r.outCheck;
  const status = r.outChecking ? "is-checking" : !oc ? "is-checking" : !oc.ok ? "is-warn is-fail" : oc.warning ? "is-warn" : "is-pass";
  return (
    <div className="case-grid">
      <section className="panel" aria-labelledby="h-rx-set">
        <header className="panel__head"><h2 id="h-rx-set">수신 설정</h2></header>
        <form className="form" noValidate onSubmit={(e) => e.preventDefault()}>
          <div className="field field--wide">
            <label htmlFor="rxOut">저장 폴더</label>
            <div className="inline">
              <input
                id="rxOut"
                type="text"
                className="mono-input"
                autoComplete="off"
                spellCheck={false}
                placeholder={`예: ${outExample(useApp.getState())}`}
                value={r.outBase}
                aria-describedby="help-rxOut"
                onChange={(e) => setRxField("outBase", e.target.value)}
                onBlur={() => void checkRxOutput()}
              />
              <button type="button" className="btn btn--ghost" onClick={() => void pickRxFolder()}>
                <Icon name="folder" />찾아보기
              </button>
            </div>
            <p className="field__help" id="help-rxOut">받은 증거는 이 폴더 아래 사건 번호{sepOf(useApp.getState())}증거 번호 폴더에 저장됩니다. 사건 번호와 증거 번호는 보내는 쪽이 정합니다.</p>
          </div>
          <div className="field">
            <label htmlFor="rxPort">대기 포트</label>
            <input
              id="rxPort"
              type="text"
              inputMode="numeric"
              className="mono-input"
              autoComplete="off"
              value={r.port}
              aria-describedby="help-rxPort"
              onChange={(e) => setRxField("port", e.target.value.replace(/\D/g, "").slice(0, 5))}
            />
            <p className="field__help" id="help-rxPort">기본 7700. 바꾸면 보내는 쪽 주소에 :포트를 붙입니다.</p>
          </div>
          <div className="field">
            <label htmlFor="rxBind">받을 네트워크</label>
            <select id="rxBind" value={r.bind} aria-describedby="help-rxBind" onChange={(e) => setRxField("bind", e.target.value)}>
              <option value="">모든 네트워크</option>
              {(useApp.getState().env?.localIps ?? []).map((ip) => (
                <option key={ip} value={ip}>{ip}</option>
              ))}
            </select>
            <p className="field__help" id="help-rxBind">증거망(분석망) 랜카드 하나로만 받으려면 그 주소를 고르세요. 다른 네트워크에서는 접속할 수 없습니다.</p>
          </div>
          {r.error && (
            <div className="field field--wide">
              <div className="infobar infobar--warn" role="alert"><Icon name="alert" /><p>{r.error}</p></div>
            </div>
          )}
        </form>
        <ul className="checks checks--inline">
          <li className={`check ${status}`}>
            <Icon name={oc?.ok && !oc.warning ? "check-c" : oc ? "alert" : "info"} className="check__ic" />
            <span>
              <b>저장 위치</b>
              <small>
                {r.outChecking
                  ? "확인하는 중…"
                  : oc
                    ? oc.warning
                      ? `${oc.message}. ${oc.warning}`
                      : oc.message
                    : "저장 폴더를 고르면 매체와 여유 공간을 확인합니다"}
              </small>
            </span>
          </li>
        </ul>
      </section>
      <div className="stack">
        <HowTo addr="" code="" />
        <Notes />
      </div>
    </div>
  );
}

function Waiting() {
  const r = useApp((s) => s.rx);
  const first = r.addrs[0] ?? "";
  const warns = r.events.filter((e) => e.level !== "info");
  return (
    <div className="case-grid">
      <section className="panel code-card" aria-labelledby="h-code">
        <div className="code-card__status" role="status">
          <span className="live-dot" aria-hidden="true" />
          접속을 기다리는 중
        </div>
        <div>
          <h2 id="h-code" className="code-card__label">연결 코드</h2>
          <div className="code-card__row">
            <b className="code-card__code mono" aria-hidden="true">{r.code}</b>
            <span className="visually-hidden">연결 코드 {r.code.split("").join(" ")}</span>
            <button type="button" className="btn btn--ghost" onClick={() => void copy(r.code, "연결 코드")}>
              <Icon name="copy" />복사
            </button>
          </div>
        </div>
        <div>
          <h3 className="code-card__label">이 PC 주소</h3>
          {r.addrs.length === 0 && <p className="code-card__note">주소를 확인하지 못했습니다. 명령 프롬프트에서 ipconfig로 확인하세요.</p>}
          {r.addrs.map((a) => (
            <div key={a} className="code-card__row">
              <b className="code-card__addr mono">{a}</b>
              <button type="button" className="icon-btn" aria-label={`${a} 복사`} onClick={() => void copy(sendAddr(a, r.port), "주소")}>
                <Icon name="copy" className="" />
              </button>
            </div>
          ))}
        </div>
        <p className="code-card__note">
          저장 폴더 <span className="mono">{r.outBase}</span>. 코드는 이번 수신에만 쓰입니다. 보내는 쪽 화면에 직접 입력하고, 메신저 등으로 보내지 마세요.
        </p>
        {warns.length > 0 && (
          <ul className="code-card__warns">
            {warns.slice(-3).map((w, i) => (
              <li key={i}><Icon name="alert" />{w.message}</li>
            ))}
          </ul>
        )}
      </section>
      <div className="stack">
        <HowTo addr={sendAddr(first, r.port)} code={r.code} />
        <Notes />
      </div>
    </div>
  );
}

function FilePill({ f }: { f: RxFile }) {
  if (f.state === "saved") return <span className="pill pill--ok"><Icon name="check" />저장됨</span>;
  if (f.state === "incomplete") return <span className="pill pill--stop">끝까지 받지 못함</span>;
  return <span className="pill pill--run">받는 중</span>;
}

function Session() {
  const r = useApp((s) => s.rx);
  const logRef = useRef<HTMLOListElement>(null);
  const stick = useRef(true);
  useEffect(() => {
    const el = logRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [r.events.length]);

  const active = r.phase === "receiving";
  const pct = r.phase === "done" ? 100 : r.need ? Math.min(100, Math.floor((r.total / r.need) * 100)) : 0;
  const vpct = r.verify && r.verify.total ? Math.floor((r.verify.done / r.verify.total) * 100) : 0;
  const report = r.outcome?.files.find(([n]) => n.endsWith("_report.html"))?.[0];
  const sep = r.evidenceDir.includes("\\") ? "\\" : "/";
  const reveal = (p: string) => api.reveal(p).catch((e) => toast(String(e)));

  return (
    <>
      {r.phase === "done" && (
        // 수신 측은 원본 해시를 모르므로 성공(초록)으로 단정하지 않음. 판정은 보고서, 보내는 쪽 화면
        <div className={`infobar ${r.outcome?.incomplete.length ? "infobar--warn" : "infobar--info"}`} role="status">
          <Icon name={r.outcome?.incomplete.length ? "alert" : "info"} />
          <p>
            <b>수신을 마쳤습니다{r.outcome?.resumes ? ` (연결 끊김 ${r.outcome.resumes}번, 모두 이어받음)` : ""}.</b> 원본 해시와 일치하는지는 이 PC가 아니라 보내는 쪽이 비교합니다. 보고서의 검증 결과를 확인하세요.
            {" "}보내는 쪽이 알린 상태: {r.outcome?.status}
          </p>
        </div>
      )}
      {r.phase === "receiving" && r.waitUntil && (
        <div className="infobar infobar--warn" role="status">
          <Icon name="alert" />
          <p>
            <b>연결이 끊어졌습니다.</b> 보내는 쪽이 같은 코드로 다시 연결하면 받은 지점부터 이어받습니다.
            {" "}{new Date(r.waitUntil).toTimeString().slice(0, 5)}까지 기다립니다. 받은 파일은 그대로 있습니다.
          </p>
        </div>
      )}
      {(r.phase === "stopped" || r.phase === "failed") && (
        <div className="infobar infobar--warn" role="alert">
          <Icon name="alert" />
          <p>
            <b>{r.phase === "stopped" ? "수신을 중지했습니다." : "수신이 끝나지 못했습니다."}</b> {r.message ?? ""} 받다 만 파일은 이름 뒤에 .incomplete가 붙어 있어 증거로 쓸 수 없습니다.
          </p>
        </div>
      )}

      <section className={`panel overall${r.phase === "done" ? " is-done" : ""}${r.phase === "stopped" || r.phase === "failed" ? " is-stop" : ""}`} aria-labelledby="h-rx-overall">
        <div className="overall__top">
          <h2 id="h-rx-overall">{r.verify ? "수신 측 검증 (다시 읽기)" : "받은 양"}</h2>
          <b className="overall__pct">{r.verify ? vpct : pct}%</b>
        </div>
        <div className="progress progress--lg" role="progressbar" aria-labelledby="h-rx-overall" aria-valuemin={0} aria-valuemax={100} aria-valuenow={r.verify ? vpct : pct}>
          <div className="progress__fill" style={{ transform: `scaleX(${(r.verify ? vpct : pct) / 100})` }} />
        </div>
        <p className="overall__sub">
          {r.verify
            ? `저장한 파일을 캐시 없이 다시 읽는 중: ${size(r.verify.done)} / ${size(r.verify.total)}`
            : `${size(r.total)} / ${size(r.need)}${active && r.rate ? `, ${rate(r.rate)}` : ""}`}
          {r.evidenceDir && <span className="mono overall__path">{r.evidenceDir}</span>}
        </p>
      </section>

      {r.case && (
        <dl className="rx-case">
          <div><dt>사건 번호</dt><dd>{r.case.caseNumber}</dd></div>
          <div><dt>증거 번호</dt><dd>{r.case.evidenceNumber}</dd></div>
          <div><dt>수집자</dt><dd>{r.case.examiner}{r.case.organization ? ` (${r.case.organization})` : ""}</dd></div>
          <div><dt>보낸 시스템</dt><dd>{r.host} <span className="mono">{r.peer}</span></dd></div>
        </dl>
      )}

      <div className="run-grid">
        <section className="panel" aria-labelledby="h-rx-files">
          <header className="panel__head">
            <h2 id="h-rx-files">받은 파일</h2>
            <span className="count">{r.files.length}개</span>
            {r.phase === "done" && (
              <span className="actions">
                <button type="button" className="btn btn--ghost" onClick={() => reveal(r.evidenceDir)}><Icon name="folder" />저장 폴더 열기</button>
                {report && <button type="button" className="btn btn--primary" onClick={() => reveal(`${r.evidenceDir}${sep}${report}`)}><Icon name="file" />보고서 파일 보기</button>}
              </span>
            )}
          </header>
          <ul className="files files--rx">
            {r.files.map((f) => (
              <li key={f.name}>
                <Icon name="file" />
                <span className="mono" title={f.name}>{f.name}</span>
                <span className="files__meta">{f.state === "recv" ? "" : size(f.size)} <FilePill f={f} /></span>
              </li>
            ))}
            {r.files.length === 0 && <li className="files__empty"><span>파일을 기다리는 중…</span></li>}
          </ul>
        </section>
        <section className="panel" aria-labelledby="h-rx-log">
          <header className="panel__head"><h2 id="h-rx-log">수신 기록</h2></header>
          <ol
            className="log"
            ref={logRef}
            tabIndex={0}
            aria-label="수신 기록"
            onScroll={(e) => {
              const el = e.currentTarget;
              stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
            }}
          >
            {r.events.map((l, i) => (
              <li key={i} className={l.level === "info" ? undefined : "is-warn"}>
                <time>{l.time.slice(11, 19)}</time>
                <span>{l.message}</span>
              </li>
            ))}
          </ol>
        </section>
      </div>
    </>
  );
}

export function ReceiveScreen() {
  const phase = useApp((s) => s.rx.phase);
  return (
    <section className="screen" aria-labelledby="h-rx">
      <header className="page-head">
        <h1 id="h-rx" tabIndex={-1}>네트워크 수신</h1>
        <p>다른 PC의 WS Collector가 보내는 증거를 이 PC에 저장합니다. 보내는 쪽(대상 시스템) 디스크에는 아무것도 쓰지 않습니다.</p>
      </header>
      {(phase === "idle" || phase === "starting") && <Setup />}
      {phase === "listening" && <Waiting />}
      {(phase === "receiving" || phase === "done" || phase === "stopped" || phase === "failed") && <Session />}
    </section>
  );
}
