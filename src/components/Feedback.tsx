import { useEffect, useRef, useState } from "react";
import { confirmCancel, confirmStopReceive, useApp } from "../store";

export function Toast() {
  const t = useApp((s) => s.toast);
  const [on, setOn] = useState(false);
  useEffect(() => {
    if (!t) return;
    setOn(true);
    const h = setTimeout(() => setOn(false), 3200);
    return () => clearTimeout(h);
  }, [t]);
  return (
    <div className={`toast${on ? " is-on" : ""}`} role="status" aria-live="polite">
      {t?.msg}
    </div>
  );
}

export function CancelDialog() {
  const open = useApp((s) => s.cancelOpen);
  const ref = useRef<HTMLDialogElement>(null);
  const keepRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      d.showModal();
      // 기본 포커스는 안전한 "계속 수집"
      keepRef.current?.focus();
    }
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog className="dialog" ref={ref} aria-labelledby="h-cancel" onClose={() => useApp.setState({ cancelOpen: false })}>
      <h2 id="h-cancel">수집을 중단할까요?</h2>
      <p>지금까지 읽은 부분은 남지만 해시가 완성되지 않아 증거로 쓸 수 없습니다. 중단 사실은 로그에 기록됩니다.</p>
      <div className="dialog__actions">
        <button type="button" className="btn btn--danger" onClick={() => void confirmCancel()}>
          수집 중단
        </button>
        <button type="button" className="btn btn--ghost" ref={keepRef} onClick={() => useApp.setState({ cancelOpen: false })}>
          계속 수집
        </button>
      </div>
    </dialog>
  );
}

export function StopReceiveDialog() {
  const open = useApp((s) => s.rx.stopOpen);
  const receiving = useApp((s) => s.rx.phase === "receiving");
  const ref = useRef<HTMLDialogElement>(null);
  const keepRef = useRef<HTMLButtonElement>(null);
  const close = () => useApp.setState((s) => ({ rx: { ...s.rx, stopOpen: false } }));
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      d.showModal();
      keepRef.current?.focus();
    }
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog className="dialog" ref={ref} aria-labelledby="h-stop-rx" onClose={close}>
      <h2 id="h-stop-rx">수신을 중지할까요?</h2>
      <p>
        {receiving
          ? "보내는 쪽 수집도 실패로 끝납니다. 받다 만 파일은 이름 뒤에 .incomplete가 붙어 증거로 쓸 수 없습니다."
          : "연결 코드가 더는 쓰이지 않습니다. 다시 받으려면 새 코드로 수신을 시작해야 합니다."}
      </p>
      <div className="dialog__actions">
        <button type="button" className="btn btn--danger" onClick={() => void confirmStopReceive()}>
          수신 중지
        </button>
        <button type="button" className="btn btn--ghost" ref={keepRef} onClick={close}>
          계속 받기
        </button>
      </div>
    </dialog>
  );
}
