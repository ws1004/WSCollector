// 시험용 화면 중계기 (Linux GUI 점검용, 외부 패키지 없음).
//
//   node tools/ui-bridge.mjs <화면 포트> <도구 포트>
//
// - 시험 빌드(VITE_WS_BRIDGE=<화면 포트>로 만든 화면)가 GET /events (SSE)로 붙어 명령을 받고, POST /result로 결과를 보냄
// - tools/ui-check.mjs bridge:<도구 포트> 가 POST /cmd {js}로 명령을 보내고 결과를 기다림
// - 127.0.0.1에서만 대기 (시험 환경 전용)

import http from "node:http";

const [pagePort, toolPort] = process.argv.slice(2).map(Number);
if (!pagePort || !toolPort) {
  console.error("사용법: node tools/ui-bridge.mjs <화면 포트> <도구 포트>");
  process.exit(2);
}

let page = null; // SSE 응답 객체
const waiting = new Map(); // id → 도구 쪽 응답 객체
let seq = 0;

const body = (req) =>
  new Promise((r) => {
    let s = "";
    req.on("data", (c) => (s += c));
    req.on("end", () => r(s));
  });

http
  .createServer(async (req, res) => {
    res.setHeader("Access-Control-Allow-Origin", "*");
    res.setHeader("Access-Control-Allow-Headers", "*");
    if (req.method === "OPTIONS") return res.end();
    if (req.url === "/events") {
      res.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache", Connection: "keep-alive" });
      res.write(": hello\n\n");
      page = res;
      console.log("화면 연결됨");
      req.on("close", () => {
        if (page === res) page = null;
      });
      return;
    }
    if (req.url === "/result" && req.method === "POST") {
      let r;
      try {
        r = JSON.parse(await body(req));
      } catch {
        return res.writeHead(400).end();
      }
      const w = waiting.get(r.id);
      if (w) {
        waiting.delete(r.id);
        w.end(JSON.stringify(r));
      }
      return res.end("ok");
    }
    res.writeHead(404).end();
  })
  .listen(pagePort, "127.0.0.1");

http
  .createServer(async (req, res) => {
    if (req.url === "/cmd" && req.method === "POST") {
      // 웹 페이지가 몰래 보내는 요청 차단: 브라우저는 다른 출처로 application/json을 사전 확인 없이 못 보내고,
      // 이 포트는 사전 확인(CORS)에 답하지 않음
      if (req.headers["content-type"] !== "application/json" || req.headers.origin) return res.writeHead(403).end();
      let js;
      try {
        ({ js } = JSON.parse(await body(req)));
      } catch {
        return res.writeHead(400).end();
      }
      if (!page) return res.end(JSON.stringify({ ok: false, error: "화면이 연결되지 않았습니다" }));
      const id = ++seq;
      waiting.set(id, res);
      page.write(`data: ${JSON.stringify({ id, js })}\n\n`);
      setTimeout(() => {
        if (waiting.has(id)) {
          waiting.delete(id);
          res.end(JSON.stringify({ ok: false, error: "화면 응답 시간 초과" }));
        }
      }, 30000);
      return;
    }
    if (req.url === "/status") return res.end(JSON.stringify({ page: !!page }));
    res.writeHead(404).end();
  })
  .listen(toolPort, "127.0.0.1");

console.log(`중계기 대기: 화면 ${pagePort}, 도구 ${toolPort}`);
