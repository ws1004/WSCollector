// 시험 빌드 전용 화면 점검 통로 (Linux WebKitGTK에는 원격 디버깅이 없어서 대신 씀).
// VITE_WS_BRIDGE=<포트> 로 화면을 빌드했을 때만 켜지고, 일반 빌드에서는 이 코드가 통째로 빠짐.
// tools/ui-bridge.mjs가 127.0.0.1:<포트>에서 명령을 보내면 실행하고 결과를 돌려줌.

export function startDevBridge() {
  const port = import.meta.env.VITE_WS_BRIDGE as string | undefined;
  if (!port) return;
  const base = `http://127.0.0.1:${port}`;
  const connect = () => {
    const es = new EventSource(`${base}/events`);
    es.onmessage = async (m) => {
      const { id, js } = JSON.parse(m.data) as { id: number; js: string };
      let out: { id: number; ok: boolean; value?: unknown; error?: string };
      try {
        // eslint-disable-next-line @typescript-eslint/no-implied-eval
        const value = await new Function(`return (async () => { ${js} })()`)();
        out = { id, ok: true, value };
      } catch (e) {
        out = { id, ok: false, error: String(e) };
      }
      void fetch(`${base}/result`, { method: "POST", body: JSON.stringify(out) });
    };
    es.onerror = () => {
      es.close();
      setTimeout(connect, 1000);
    };
  };
  connect();
}
