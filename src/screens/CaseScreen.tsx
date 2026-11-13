import { useEffect, useState } from "react";
import { checkOutput, fieldError, isLinux, loadEnvironment, outExample, pickFolder, sepOf, setDest, setField, useApp, type CaseForm, type Dest } from "../store";
import { Icon } from "../components/Icon";
import { Seg } from "../components/Seg";
import type { Check } from "../types";

function Field(props: {
  id: keyof CaseForm;
  label: string;
  placeholder: string;
  help?: string;
  autoComplete?: string;
  required?: boolean;
  upper?: boolean;
  wide?: boolean;
  mono?: boolean;
  onBlur?: () => void;
  children?: React.ReactNode;
}) {
  const value = useApp((s) => s.form[props.id]);
  const err = useApp((s) => s.errors[props.id]);
  const helpId = `help-${props.id}`;
  const input = (
    <input
      id={props.id}
      name={props.id}
      type="text"
      className={[props.mono && "mono-input", props.upper && "is-upper"].filter(Boolean).join(" ") || undefined}
      autoComplete={props.autoComplete ?? "off"}
      spellCheck={false}
      required={props.required}
      placeholder={props.placeholder}
      value={value}
      aria-invalid={err ? true : undefined}
      aria-describedby={helpId}
      onChange={(e) => setField(props.id, e.target.value)}
      onBlur={props.onBlur}
    />
  );
  return (
    <div className={`field${props.wide ? " field--wide" : ""}`}>
      <label htmlFor={props.id}>{props.label}</label>
      {props.children ? <div className="inline">{input}{props.children}</div> : input}
      <p className={`field__help${err ? " is-error" : ""}`} id={helpId}>
        {err ?? props.help ?? ""}
      </p>
    </div>
  );
}

function PathPreview() {
  const f = useApp((s) => s.form);
  const net = useApp((s) => s.dest === "net");
  const sp = useApp((s) => sepOf(s));
  const base = f.outBase.trim().replace(/[\\/]+$/, "");
  const addr = f.netAddr.trim();
  const part = (v: string, cls: string, ph: string) =>
    v.trim() ? <span className={cls}>{v.trim()}</span> : <span className={`${cls} is-empty`}>{ph}</span>;
  return (
    <div className="field field--wide">
      <div className="path-preview" aria-live="polite">
        <span className="path-preview__label">{net ? `증거 파일이 저장될 경로 (수신 PC${addr ? ` ${addr}` : ""})` : "증거 파일이 저장될 경로"}</span>
        <p className="path-preview__path mono">
          {net ? <span className="seg-base is-empty">수신 PC의 저장 폴더</span> : part(base, "seg-base", "저장 폴더")}{sp}{part(f.caseNo, "seg-case", "사건 번호")}{sp}{part(f.evNo, "seg-ev", "증거 번호")}
        </p>
        <p className="path-preview__note">
          {net
            ? "이미지, 로그, 보고서를 모두 수신 PC에 저장합니다. 이 PC의 디스크에는 쓰지 않습니다."
            : "사건 번호와 증거 번호 폴더는 수집을 시작할 때 자동으로 만들어집니다."}
        </p>
      </div>
    </div>
  );
}

function DestFields() {
  const dest = useApp((s) => s.dest);
  const linux = useApp((s) => isLinux(s));
  const example = useApp((s) => outExample(s));
  return (
    <>
      <div className="field field--wide">
        <span className="field__label" aria-hidden="true">저장 위치</span>
        <Seg<Dest>
          name="dest"
          label="저장 위치"
          value={dest}
          options={[
            { v: "local", label: "이 PC에 연결한 저장 매체" },
            { v: "net", label: "네트워크 수신 PC" },
          ]}
          onChange={setDest}
        />
      </div>
      {dest === "local" ? (
        <Field
          id="outBase"
          label="저장 폴더"
          placeholder={`예: ${example}`}
          help={linux ? "라이브 수집이면 대상 시스템의 내장 디스크가 아닌 외부 매체(USB 등)가 마운트된 폴더를 고르세요." : "라이브 수집이면 대상 PC의 내장 디스크가 아닌 외부 매체의 폴더를 고르세요."}
          required
          wide
          mono
          onBlur={() => void checkOutput()}
        >
          <button type="button" className="btn btn--ghost" onClick={() => void pickFolder()}>
            <Icon name="folder" />찾아보기
          </button>
        </Field>
      ) : (
        <>
          <Field id="netAddr" label="수신 PC 주소" placeholder="예: 192.168.0.10" help="수신 PC 화면의 주소. 포트를 바꿨으면 :포트를 붙입니다." required mono />
          <Field id="netCode" label="연결 코드" placeholder="예: 4KQ7M-2XD9P" help="수신 PC 화면에 나온 10자리 코드입니다." required mono upper />
        </>
      )}
    </>
  );
}

function useClock(start: string | undefined, at: number) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const h = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(h);
  }, []);
  // 기준은 백엔드에서 시각을 받은 순간 (화면을 다시 그린 순간이 아님: 다른 화면에 다녀와도 밀리지 않게)
  const base = { at };
  if (!start) return "";
  // 백엔드가 알려준 대상 시스템 시각 + 경과 시간
  const t0 = Date.parse(start.slice(0, 19));
  if (Number.isNaN(t0)) return start;
  const d = new Date(t0 + (now - base.at));
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function CheckRow({ c }: { c: Check }) {
  const icon = c.status === "pass" ? "check-c" : c.status === "pending" ? "info" : "alert";
  const cls = c.status === "pass" ? "is-pass" : c.status === "pending" ? "is-checking" : c.status === "fail" ? "is-warn is-fail" : "is-warn";
  return (
    <li className={`check ${cls}`}>
      <svg className="check__ic" aria-hidden="true">
        <use href={`#i-${icon}`} />
      </svg>
      <span>
        <b>{c.title}</b>
        <small className={c.id === "exe" || c.id === "temp" ? "mono" : undefined}>{c.detail}</small>
      </span>
    </li>
  );
}

export function CaseScreen() {
  const env = useApp((s) => s.env);
  const envError = useApp((s) => s.envError);
  const outCheck = useApp((s) => s.outCheck);
  const outChecking = useApp((s) => s.outChecking);
  const outBase = useApp((s) => s.form.outBase);
  const dest = useApp((s) => s.dest);
  const netAddr = useApp((s) => s.form.netAddr);
  const netCode = useApp((s) => s.form.netCode);
  const [rechecking, setRechecking] = useState(false);
  const envAt = useApp((s) => s.envAt);
  const clock = useClock(env?.host.localTime, envAt);

  const netReady = !fieldError("netAddr", netAddr) && !fieldError("netCode", netCode);
  const outRow: Check = dest === "net"
    ? {
        id: "out",
        title: "저장 위치",
        detail: netReady
          ? `네트워크 수신 PC ${netAddr.trim()}. 수집을 시작할 때 연결하고, 수신 PC가 여유 공간을 확인합니다`
          : "수신 PC 주소와 연결 코드를 입력하세요",
        status: netReady ? "pass" : "pending",
      }
    : outChecking
    ? { id: "out", title: "저장 위치", detail: "확인하는 중…", status: "pending" }
    : outCheck
      ? {
          id: "out",
          title: "저장 위치",
          detail: outCheck.warning ? `${outCheck.message}. ${outCheck.warning}` : outCheck.message,
          status: !outCheck.ok ? "fail" : outCheck.warning ? "warn" : "pass",
        }
      : { id: "out", title: "저장 위치", detail: outBase.trim() ? "저장 폴더를 확인하려면 입력란을 벗어나세요" : "저장 폴더를 고르면 확인합니다", status: "pending" };
  const checks = env ? [env.checks[0], env.checks[1], outRow, ...env.checks.slice(2)] : [];

  return (
    <section className="screen" aria-labelledby="h-case">
      <header className="page-head">
        <h1 id="h-case" tabIndex={-1}>사건 정보</h1>
        <p>보고서와 증거 파일 이름에 들어갈 정보입니다. 수집을 시작하면 바꿀 수 없습니다.</p>
      </header>

      <div className="case-grid">
        <section className="panel" aria-labelledby="h-form">
          <header className="panel__head"><h2 id="h-form">사건과 수집자</h2></header>
          <form className="form" noValidate onSubmit={(e) => e.preventDefault()}>
            <Field id="caseNo" label="사건 번호" placeholder="예: 2026-형제-01187" help="저장 폴더 이름으로 사용됩니다." required />
            <Field id="evNo" label="증거 번호" placeholder="예: EV-003" help="하위 폴더와 파일 이름에 사용됩니다." required />
            <Field id="examiner" label="수집자" placeholder="예: 홍길동" autoComplete="name" required />
            <Field id="org" label="소속" placeholder="예: 디지털포렌식팀" autoComplete="organization" />
            <DestFields />
            <PathPreview />
            <div className="field field--wide">
              <label htmlFor="notes">현장 메모</label>
              <textarea
                id="notes"
                name="notes"
                rows={3}
                placeholder="예: 피의자 사무실 책상 위 데스크톱, 전원 켜진 상태로 발견…"
                value={useApp((s) => s.form.notes)}
                onChange={(e) => setField("notes", e.target.value)}
              />
            </div>
          </form>
        </section>

        <div className="stack">
          <section className="panel" aria-labelledby="h-env">
            <header className="panel__head">
              <h2 id="h-env">수집 환경 점검</h2>
              <button
                type="button"
                className="btn btn--subtle"
                disabled={rechecking}
                onClick={async () => {
                  setRechecking(true);
                  await Promise.all([loadEnvironment(), checkOutput()]);
                  setRechecking(false);
                  useApp.setState({ toast: { id: Date.now(), msg: "환경 점검을 다시 마쳤습니다." } });
                }}
              >
                <Icon name="refresh" />{rechecking ? "점검 중…" : "다시 점검"}
              </button>
            </header>
            {envError && <p className="field__help is-error" style={{ padding: "12px 20px" }}>환경 정보를 읽지 못했습니다: {envError}</p>}
            <ul className="checks">
              {checks.map((c) => (
                <CheckRow key={c.id} c={c} />
              ))}
              {!env && !envError && <li className="check is-checking"><span><b>점검하는 중…</b></span></li>}
            </ul>
          </section>

          <section className="panel clock" aria-labelledby="h-clock">
            <Icon name="clock" className="clock__ic" />
            <div>
              <h2 id="h-clock">대상 시스템 시각</h2>
              <p className="clock__time mono">{clock || "…"}</p>
              <p className="clock__sub">{env ? `${env.host.timezone}. 보고서에 그대로 기록됩니다.` : ""}</p>
            </div>
          </section>
        </div>
      </div>
    </section>
  );
}
