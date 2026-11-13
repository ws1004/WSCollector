// 개발용 화면 점검 도구: 실행 중인 WS Collector(개발, 시험 빌드)의 화면에 접속해
// 입력, 클릭, 대기, 화면 캡처를 순서대로 실행합니다.
//
// 두 가지 연결 방식:
// 1) Windows (WebView2 원격 디버깅): (관리자 터미널) set WS_DEBUG_PORT=9333 && npm run tauri dev
//      node tools/ui-check.mjs 9333 type "#caseNo" "2026-TEST-002" click ".bar .btn--primary" wait 800 shot out.png
// 2) Linux (WebKitGTK는 원격 디버깅이 없음): 시험 빌드의 화면이 tools/ui-bridge.mjs에 붙음
//      node tools/ui-bridge.mjs 9334 9335 &   (화면 ↔ 9334, 이 도구 ↔ 9335)
//      UI_SHOT_CMD="import -display :99 -window root {file}" node tools/ui-check.mjs bridge:9335 ... shot out.png
//
// 명령: type <선택자> <값> | click <선택자> | clickText <글자> | wait <ms> | waitFor <선택자> [ms] | shot <파일> | eval <JS>

import { writeFileSync } from "node:fs";
import { execSync } from "node:child_process";

const [port, ...args] = process.argv.slice(2);
if (!port) {
  console.error("사용법: node tools/ui-check.mjs <포트|bridge:포트> <명령...>");
  process.exit(2);
}

let evaluate, screenshot, close;

if (port.startsWith("bridge:")) {
  const base = `http://127.0.0.1:${port.slice(7)}`;
  evaluate = async (expression) => {
    const r = await (await fetch(`${base}/cmd`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ js: `return (${expression});` }) })).json();
    if (!r.ok) throw new Error(r.error ?? "평가 오류");
    return r.value;
  };
  screenshot = (file) => {
    const cmd = (process.env.UI_SHOT_CMD ?? "import -display :99 -window root {file}").replace("{file}", JSON.stringify(file));
    execSync(cmd);
  };
  close = () => {};
} else {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const page = targets.find((t) => t.type === "page" && /localhost|tauri/.test(t.url)) ?? targets.find((t) => t.type === "page");
  if (!page) throw new Error("웹뷰 페이지를 찾지 못했습니다");
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  let seq = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pending.has(d.id)) {
      pending.get(d.id)(d);
      pending.delete(d.id);
    }
  };
  const send = (method, params = {}) =>
    new Promise((r) => {
      const id = ++seq;
      pending.set(id, r);
      ws.send(JSON.stringify({ id, method, params }));
    });
  evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description ?? "평가 오류");
    return r.result?.result?.value;
  };
  screenshot = async (file) => {
    const r = await send("Page.captureScreenshot", { format: "png" });
    writeFileSync(file, Buffer.from(r.result.data, "base64"));
  };
  close = () => ws.close();
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const q = (s) => JSON.stringify(s);

for (let i = 0; i < args.length; i++) {
  const cmd = args[i];
  if (cmd === "type") {
    const sel = args[++i], val = args[++i];
    const ok = await evaluate(`(() => { const el = document.querySelector(${q(sel)}); if (!el) return false;
      const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, "value").set.call(el, ${q(val)});
      el.dispatchEvent(new Event("input", { bubbles: true })); el.focus(); el.blur(); return true; })()`);
    console.log(`type ${sel}: ${ok ? "ok" : "없음"}`);
  } else if (cmd === "click") {
    const sel = args[++i];
    const ok = await evaluate(`(() => { const el = document.querySelector(${q(sel)}); if (!el) return false; el.click(); return true; })()`);
    console.log(`click ${sel}: ${ok ? "ok" : "없음"}`);
  } else if (cmd === "clickText") {
    const text = args[++i];
    const ok = await evaluate(`(() => { const el = [...document.querySelectorAll("button, a, label")].find(e => e.textContent.trim().includes(${q(text)})); if (!el) return false; el.click(); return true; })()`);
    console.log(`clickText ${text}: ${ok ? "ok" : "없음"}`);
  } else if (cmd === "wait") {
    await sleep(Number(args[++i]));
  } else if (cmd === "waitFor") {
    const sel = args[++i];
    const limit = Number(args[i + 1]) || 60000;
    if (/^\d+$/.test(args[i + 1] ?? "")) i++;
    const t0 = Date.now();
    let found = false;
    while (Date.now() - t0 < limit) {
      if (await evaluate(`!!document.querySelector(${q(sel)})`)) { found = true; break; }
      await sleep(250);
    }
    console.log(`waitFor ${sel}: ${found ? "ok" : "시간 초과"}`);
  } else if (cmd === "shot") {
    const file = args[++i];
    await screenshot(file);
    console.log(`shot ${file}`);
  } else if (cmd === "eval") {
    console.log(JSON.stringify(await evaluate(args[++i]), null, 2));
  } else {
    console.error(`알 수 없는 명령: ${cmd}`);
    process.exit(2);
  }
}
close();
