import { useEffect, useRef } from "react";
import { api } from "./api";
import { loadEnvironment, onCollect, onFinish, onReceive, onReceiveFinish, toast, useApp } from "./store";
import { IconSprite } from "./components/Icon";
import { Sidebar } from "./components/Sidebar";
import { BottomBar } from "./components/BottomBar";
import { CancelDialog, StopReceiveDialog, Toast } from "./components/Feedback";
import { CaseScreen } from "./screens/CaseScreen";
import { SelectScreen } from "./screens/SelectScreen";
import { RunScreen } from "./screens/RunScreen";
import { DoneScreen } from "./screens/DoneScreen";
import { ReceiveScreen } from "./screens/ReceiveScreen";

export default function App() {
  const screen = useApp((s) => s.screen);
  const mode = useApp((s) => s.mode);
  const view = mode === "receive" ? "receive" : screen;
  const testMax = useApp((s) => s.env?.testMaxBytes);
  const mainRef = useRef<HTMLElement>(null);
  const first = useRef(true);

  useEffect(() => {
    void loadEnvironment();
    // 개발 모드 StrictMode에서 두 번 실행돼도 이벤트를 한 번만 받도록 해제를 기다림
    let alive = true;
    const subs = [
      api.onCollect((e) => alive && onCollect(e)),
      api.onFinish((e) => alive && onFinish(e)),
      api.onReceive((e) => alive && onReceive(e)),
      api.onReceiveFinish((e) => alive && onReceiveFinish(e)),
      api.onCloseBlocked((what) => alive && toast(what === "receive" ? "수신 중에는 창을 닫을 수 없습니다. 먼저 수신을 중지하세요." : "수집 중에는 창을 닫을 수 없습니다. 먼저 수집을 중단하세요.")),
    ];
    // 브라우저 기본 단축키(새로고침, 찾기, 인쇄 등)로 화면 상태가 날아가지 않게 막음
    const block = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      if (e.key === "F5" || ((e.ctrlKey || e.metaKey) && ["r", "p", "f", "g", "j", "u"].includes(k))) e.preventDefault();
    };
    window.addEventListener("keydown", block);
    const ctx = (e: MouseEvent) => {
      const t = e.target as HTMLElement;
      if (!t.closest("input, textarea")) e.preventDefault();
    };
    window.addEventListener("contextmenu", ctx);
    return () => {
      alive = false;
      subs.forEach((p) => p.then((un) => un()));
      window.removeEventListener("keydown", block);
      window.removeEventListener("contextmenu", ctx);
    };
  }, []);

  // 화면 전환 시 맨 위로, 제목에 포커스
  useEffect(() => {
    mainRef.current?.scrollTo(0, 0);
    if (first.current) {
      first.current = false;
      return;
    }
    const fa = useApp.getState().focusAfter;
    const target = fa ? document.getElementById(fa) : document.querySelector<HTMLElement>(".screen h1");
    target?.focus({ preventScroll: true });
    if (fa) useApp.setState({ focusAfter: null });
    document.title = `${{ case: "사건 정보", select: "수집 항목", run: "수집 진행", done: "결과", receive: "네트워크 수신" }[view]}: WS Collector`;
  }, [view]);

  return (
    <>
      <IconSprite />
      <a className="skip" href="#main" onClick={(e) => { e.preventDefault(); mainRef.current?.focus(); }}>본문으로 건너뛰기</a>
      <div className="app">
        <Sidebar />
        <main className="main" id="main" tabIndex={-1} ref={mainRef}>
          {testMax && (
            <div className="infobar infobar--warn test-banner" role="note">
              <p><b>개발 빌드 시험 모드</b> 디스크마다 앞부분 {Math.round(Number(testMax) / 1024 ** 2)} MB만 읽습니다. 실제 수집에는 릴리스 빌드를 쓰세요.</p>
            </div>
          )}
          {view === "case" && <CaseScreen />}
          {view === "select" && <SelectScreen />}
          {view === "run" && <RunScreen />}
          {view === "done" && <DoneScreen />}
          {view === "receive" && <ReceiveScreen />}
        </main>
        <BottomBar />
      </div>
      <CancelDialog />
      <StopReceiveDialog />
      <Toast />
    </>
  );
}
