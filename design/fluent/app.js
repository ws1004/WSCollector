"use strict";
/* WS Collector Fluent 디자인 데모. 장치, 시각, 해시는 모두 예시 데이터입니다. */

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const GiB = 1024 ** 3;
const size = (b) => b >= 1024 * GiB ? (b / 1024 / GiB).toFixed(2) + " TB" : b >= GiB ? (b / GiB).toFixed(2) + " GB" : Math.round(b / 1024 ** 2) + " MB";
const pad = (n) => String(n).padStart(2, "0");
const dur = (s) => { s = Math.max(0, Math.round(s)); return `${pad(Math.floor(s / 3600))}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`; };
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const icon = (id, cls = "ic") => `<svg class="${cls}" aria-hidden="true"><use href="#i-${id}"/></svg>`;

/* ---------- 예시 장치 ---------- */
const MEM = 32 * GiB, FREE = 1.71 * 1024 * GiB;
const DISKS = [
  { id: "d0", no: 0, model: "Samsung SSD 980 PRO 1TB", bus: "NVMe", kind: "내장", serial: "S5GXNF0R412873A", size: 1000204886016, icon: "disk", rate: 2.1 * GiB,
    parts: [
      { id: "d0p1", name: "EFI 시스템", fs: "FAT32", size: 104857600, used: 0.3 },
      { id: "d0p2", name: "C:", label: "Windows", fs: "NTFS", size: 999295819776, used: 0.56 },
      { id: "d0p3", name: "복구", fs: "NTFS", size: 786432000, used: 0.7 },
    ] },
  { id: "d1", no: 1, model: "WDC WD20EZAZ", bus: "SATA", kind: "내장", serial: "WD-WX12D81A7Q3N", size: 2000398934016, icon: "disk", rate: 0.18 * GiB,
    parts: [{ id: "d1p1", name: "D:", label: "자료", fs: "NTFS", size: 2000381018112, used: 0.44 }] },
  { id: "d2", no: 2, model: "SanDisk Ultra USB 3.0", bus: "USB", kind: "이동식", serial: "4C530001230715109412", size: 61505273856, icon: "usb", role: "수집 도구",
    parts: [{ id: "d2p1", name: "F:", label: "WSCOLLECTOR", fs: "exFAT", size: 61503176704, used: 0.12 }] },
  { id: "d3", no: 3, model: "Samsung PSSD T7", bus: "USB", kind: "외장", serial: "S6WSNJ0T301822K", size: 2000398934016, icon: "usb", role: "저장 위치",
    parts: [{ id: "d3p1", name: "E:", label: "T7", fs: "exFAT", size: 2000396836864, used: 0.14 }] },
  { id: "d4", no: 4, model: "Seagate ST1000LM035", bus: "USB", kind: "외장", serial: "WL1B9QXE", size: 1000204886016, icon: "disk", rate: 0.12 * GiB, warn: "원본 디스크라면 쓰기 방지 장치를 거쳐 연결하세요.",
    parts: [{ id: "d4p1", name: "G:", label: "백업", fs: "NTFS", size: 1000202788864, used: 0.78 }] },
];

const state = { sel: new Set(["d0"]), tasks: { disk: true, mem: true }, demo: "full", started: false, finished: false, cancelled: false, jobs: [] };

/* ---------- 화면 전환 (URL 해시) ---------- */
const SCREENS = ["case", "select", "run", "done"];
const TITLES = { case: "사건 정보", select: "수집 항목", run: "수집 진행", done: "결과" };
const allowed = (s) => s === "run" ? state.started : s === "done" ? state.finished : true;

function show(name, opts = {}) {
  if (!SCREENS.includes(name) || !allowed(name)) name = state.finished ? "done" : state.started ? "run" : SCREENS.includes(name) ? "select" : "case";
  if (state.started && !state.finished && !state.cancelled && (name === "case" || name === "select")) name = "run";
  const changed = document.body.dataset.screen !== name || opts.initial;
  document.body.dataset.screen = name;
  SCREENS.forEach((s) => { $("#screen-" + s).hidden = s !== name; });
  const idx = SCREENS.indexOf(name);
  $$(".steps li").forEach((li, i) => {
    const s = li.dataset.step, a = li.querySelector("a");
    const locked = !allowed(s) || (state.started && !state.finished && !state.cancelled && i < 2);
    li.classList.toggle("is-current", s === name);
    li.classList.toggle("is-done", i < idx || (s === "run" && state.finished) || (s === "done" && state.finished && name === "done"));
    li.classList.toggle("is-locked", locked);
    if (s === name) a.setAttribute("aria-current", "step"); else a.removeAttribute("aria-current");
    if (locked) { a.setAttribute("aria-disabled", "true"); a.tabIndex = -1; } else { a.removeAttribute("aria-disabled"); a.removeAttribute("tabindex"); }
  });
  if (location.hash.slice(1) !== name) history.replaceState(null, "", "#" + name);
  document.title = `${TITLES[name]}: WS Collector 데모`;
  renderBar();
  if (changed) {
    $("#main").scrollTo(0, 0);
    if (!opts.initial) { const h = $("#screen-" + name + " h1"); h.tabIndex = -1; h.focus({ preventScroll: true }); }
  }
}
window.addEventListener("hashchange", () => show(location.hash.slice(1)));

/* ---------- 하단 바: 버튼은 상태가 바뀔 때만 다시 그림 ---------- */
function setActions(key, html, bind) {
  const box = $("#barActions");
  if (box.dataset.key === key) return;
  box.dataset.key = key;
  box.innerHTML = html;
  if (bind) bind();
}
function renderBar() {
  const s = document.body.dataset.screen, sum = $("#barSum");
  if (s === "case") {
    sum.innerHTML = `<span>사건<b>${esc($("#caseNo").value || "미입력")}</b></span><span>증거<b>${esc($("#evNo").value || "미입력")}</b></span><span>수집자<b>${esc($("#examiner").value || "미입력")}</b></span>`;
    setActions("case", `<button type="button" class="btn btn--primary" id="toSelect">다음: 수집 항목</button>`, () => { $("#toSelect").onclick = submitCase; });
  } else if (s === "select") {
    const n = taskCount(), need = neededBytes(), over = need > FREE;
    const dis = n === 0 || over || (state.tasks.disk && state.demo !== "full");
    sum.innerHTML = `<div class="cap"><div class="cap__text"><span>필요한 공간 <b>${size(need)}</b></span><span class="${over ? "is-bad" : ""}">여유 공간 <b>${size(FREE)}</b>${over ? " (부족)" : ""}</span></div><div class="cap__track"><div class="cap__fill${over ? " is-over" : ""}" style="transform:scaleX(${Math.min(need / FREE, 1)})"></div></div></div>`;
    setActions(`select-${n}-${dis}`, `<a class="btn btn--ghost" href="#case">이전</a><button type="button" class="btn btn--primary" id="startBtn" ${dis ? "disabled" : ""}>${icon("play")}작업 ${n}개 시작</button>`, () => { $("#startBtn").onclick = startRun; });
  } else if (s === "run") {
    const left = state.jobs.filter((j) => j.status !== "done").length;
    sum.innerHTML = state.finished ? `<span>모든 작업을 마쳤습니다</span>` : state.cancelled ? `<span class="is-bad"><b>수집을 중단했습니다</b></span>` : `<span>남은 작업<b>${left}개</b></span><span>저장 위치<b class="mono">${esc(evidenceDir())}</b></span>`;
    if (state.finished) setActions("run-done", `<a class="btn btn--primary" href="#done">결과 보기</a>`);
    else if (state.cancelled) setActions("run-stop", `<button type="button" class="btn btn--ghost" id="resetBtn">새 수집 시작</button>`, () => { $("#resetBtn").onclick = resetAll; });
    else setActions("run-live", `<button type="button" class="btn btn--ghost" id="cancelBtn">${icon("stop")}수집 중단</button>`, () => { $("#cancelBtn").onclick = () => $("#cancelDialog").showModal(); });
  } else {
    sum.innerHTML = `<span>증거 파일<b>${state.jobs.length}개</b></span><span>해시 검증<b style="color:var(--ok)">모두 일치</b></span>`;
    setActions("done", `<button type="button" class="btn btn--ghost" id="resetBtn2">새 수집 시작</button>`, () => { $("#resetBtn2").onclick = resetAll; });
  }
}

/* ---------- 1. 사건 정보 ---------- */
/* 저장 경로 = 저장 폴더\사건 번호\증거 번호 (하위 두 폴더는 수집 시작 때 자동 생성) */
const val = (id) => $("#" + id).value.trim();
const baseDir = () => val("outPath").replace(/[\\/]+$/, "");
const evidenceDir = () => `${baseDir()}\\${val("caseNo")}\\${val("evNo")}`;
const filePrefix = () => `${val("evNo")}_DESKTOP-7Q2LM4K_20261105`;
const BAD_CHARS = /[\\/:*?"<>|]/;

function renderPathPreview() {
  const part = (v, cls, ph) => v ? `<span class="${cls}">${esc(v)}</span>` : `<span class="${cls} is-empty">${ph}</span>`;
  $("#pathPreview").innerHTML = `${part(baseDir(), "seg-base", "저장 폴더")}\\${part(val("caseNo"), "seg-case", "사건 번호")}\\${part(val("evNo"), "seg-ev", "증거 번호")}`;
}

function fieldError(el) {
  const v = el.value.trim();
  if (!v) return `${el.labels[0].textContent} 항목을 입력해야 보고서를 만들 수 있습니다.`;
  if ((el.id === "caseNo" || el.id === "evNo") && BAD_CHARS.test(v)) return `폴더 이름에 쓸 수 없는 문자(\\ / : * ? " < > |)가 들어 있습니다.`;
  if (el.id === "outPath" && !/^[A-Za-z]:\\/.test(v)) return "드라이브 문자로 시작하는 전체 경로를 입력하세요. 예: E:\\Cases";
  if (el.id === "outPath" && /^C:/i.test(v)) return "C: 드라이브는 대상 PC의 내장 디스크입니다. 외부 매체의 폴더를 고르세요.";
  return "";
}
function setFieldState(el, msg) {
  const help = el.closest(".field").querySelector(".field__help");
  if (!help.id) help.id = "help-" + el.id;
  el.setAttribute("aria-describedby", help.id);
  help.dataset.orig ??= help.textContent;
  el.setAttribute("aria-invalid", String(!!msg));
  help.textContent = msg || help.dataset.orig;
  help.classList.toggle("is-error", !!msg);
}
function submitCase() {
  let firstBad = null;
  $$("#caseForm [required]").forEach((el) => {
    const msg = fieldError(el);
    setFieldState(el, msg);
    if (msg) firstBad ??= el;
  });
  if (firstBad) { firstBad.focus(); return; }
  $("#sub-case").textContent = val("caseNo");
  location.hash = "#select";
}
$("#caseForm").addEventListener("input", (e) => {
  if (e.target.getAttribute("aria-invalid") === "true" && !fieldError(e.target)) setFieldState(e.target, "");
  renderPathPreview();
  renderBar();
});
$("#caseForm").addEventListener("submit", (e) => { e.preventDefault(); submitCase(); });
$("#browseBtn").onclick = () => {
  /* 실제 앱에서는 폴더 선택 창이 열립니다. 데모는 외장 SSD의 Cases 폴더를 고른 것으로 처리합니다. */
  $("#outPath").value = "E:\\Cases";
  setFieldState($("#outPath"), "");
  $("#outHelp").textContent = "Samsung PSSD T7, 외장, 여유 공간 1.71 TB";
  $("#outHelp").dataset.orig = $("#outHelp").textContent;
  renderPathPreview(); renderBar();
  toast("저장 폴더를 E:\\Cases 로 정했습니다.");
};
$("#recheckBtn").onclick = () => {
  const btn = $("#recheckBtn");
  if (btn.disabled) return;
  btn.disabled = true;
  const rows = $$("#checks .check");
  const saved = rows.map((r) => [r.className, r.querySelector("use").getAttribute("href")]);
  rows.forEach((r) => { r.className = "check is-checking"; r.querySelector("use").setAttribute("href", "#i-refresh"); });
  setTimeout(() => {
    rows.forEach((r, i) => { r.className = saved[i][0]; r.querySelector("use").setAttribute("href", saved[i][1]); });
    btn.disabled = false;
    toast("환경 점검을 다시 마쳤습니다. 5개 통과, 1개 주의.");
  }, 700);
};
let clockT = Date.UTC(2026, 10, 5, 5, 32, 8);
setInterval(() => {
  clockT += 1000;
  const d = new Date(clockT + 9 * 3600e3);
  $("#clock").textContent = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
}, 1000);

/* ---------- 2. 수집 항목 ---------- */
function targets() {
  const out = [];
  for (const d of DISKS) {
    if (state.sel.has(d.id)) out.push({ kind: "disk", name: `Disk ${d.no}`, desc: d.model, size: d.size, rate: d.rate, tag: `Disk${d.no}` });
    else d.parts.forEach((p) => state.sel.has(p.id) && out.push({ kind: "vol", name: `${p.name}${p.label ? " " + p.label : ""}`, desc: `Disk ${d.no}`, size: p.size, rate: d.rate, tag: p.name.replace(":", "") }));
  }
  return out;
}
const taskCount = () => (state.tasks.mem ? 1 : 0) + (state.tasks.disk ? targets().length : 0);
const neededBytes = () => (state.tasks.mem ? MEM : 0) + (state.tasks.disk ? targets().reduce((a, t) => a + t.size, 0) : 0);
const fileName = (t) => `${filePrefix()}_${t.tag}.dd`;

function renderDisks() {
  const box = $("#disks");
  if (state.demo === "loading") {
    box.setAttribute("aria-busy", "true");
    box.innerHTML = `<p class="visually-hidden">디스크 목록을 불러오는 중입니다…</p>` + Array.from({ length: 3 }, () => `<div class="skeleton" aria-hidden="true"><i></i><i></i><i></i></div>`).join("");
    $("#diskCount").textContent = "찾는 중…";
    return;
  }
  box.removeAttribute("aria-busy");
  if (state.demo === "error") {
    $("#diskCount").textContent = "";
    box.innerHTML = `<div class="state state--error">${icon("lock", "state__ic")}<h3>디스크를 읽을 권한이 없습니다</h3><p>WS Collector가 일반 사용자 권한으로 실행되었습니다. 물리 디스크와 메모리에 접근하려면 관리자 권한이 필요합니다.</p><button type="button" class="btn btn--primary" id="elevate">${icon("shield")}관리자 권한으로 다시 실행</button></div>`;
    $("#elevate").onclick = () => { setDemo("full", "관리자 권한으로 다시 실행했습니다."); $("#refreshDisks").focus(); };
    return;
  }
  if (state.demo === "empty") {
    $("#diskCount").textContent = "0개";
    box.innerHTML = `<div class="state">${icon("disk", "state__ic")}<h3>이미징할 디스크가 없습니다</h3><p>수집 도구와 저장 위치 외에는 연결된 디스크가 없습니다. 원본 디스크를 쓰기 방지 장치에 연결한 뒤 다시 찾아보세요.</p><button type="button" class="btn btn--ghost" id="rescan">${icon("refresh")}디스크 다시 찾기</button></div>`;
    $("#rescan").onclick = () => { setDemo("full", "디스크 5개를 찾았습니다."); $("#refreshDisks").focus(); };
    return;
  }
  $("#diskCount").textContent = `${DISKS.length}개 발견`;
  box.innerHTML = DISKS.map((d) => {
    const on = state.sel.has(d.id), locked = !!d.role;
    const segs = d.parts.map((p, i) => `<span class="map__seg map__seg--${i % 4}" style="flex-grow:${Math.max(p.size / d.size, 0.035)}"></span>`).join("");
    const parts = d.parts.map((p, i) => {
      const inc = on, pon = inc || state.sel.has(p.id);
      return `<label class="part${pon ? " is-on" : ""}${inc ? " is-inc" : ""}${locked ? " is-locked" : ""}">
        <input type="checkbox" data-id="${p.id}" ${pon ? "checked" : ""} ${inc || locked ? "disabled" : ""} aria-label="${esc(p.name)} ${esc(p.label || "")} 볼륨${inc ? ", 디스크 전체에 포함" : ""}">
        <span class="box" aria-hidden="true"><svg><use href="#i-check"/></svg></span>
        <span class="part__swatch map__seg--${i % 4}" aria-hidden="true"></span>
        <span class="part__name"><b>${esc(p.name)}</b>${p.label ? ` ${esc(p.label)}` : ""}</span>
        <span class="part__fs">${p.fs}</span>
        <span class="part__use" aria-hidden="true"><i style="width:${p.used * 100}%"></i></span>
        <span class="part__size">${size(p.size)}</span>
      </label>`;
    }).join("");
    return `<article class="disk${on ? " is-on" : ""}${locked ? " is-locked" : ""}">
      <label class="disk__head">
        <input type="checkbox" data-id="${d.id}" ${on ? "checked" : ""} ${locked ? "disabled" : ""} aria-label="Disk ${d.no} ${esc(d.model)} 전체">
        <span class="box" aria-hidden="true"><svg><use href="#i-check"/></svg></span>
        <span class="disk__icon">${icon(d.icon, "")}</span>
        <span class="disk__title"><b>Disk ${d.no}</b><span>${esc(d.model)}</span></span>
        <span class="disk__tags">${d.role ? `<span class="tag tag--role">${icon("lock")}${d.role}</span>` : ""}<span class="tag">${d.bus}</span><span class="tag">${d.kind}</span></span>
        <span class="disk__size">${size(d.size)}</span>
      </label>
      <div class="disk__meta mono" translate="no">\\\\.\\PHYSICALDRIVE${d.no}<span>S/N ${d.serial}</span></div>
      <div class="map" aria-hidden="true">${segs}</div>
      ${d.warn ? `<p class="disk__warn">${icon("alert")}${d.warn}</p>` : ""}
      ${d.role ? `<p class="disk__note">${d.role === "수집 도구" ? "WS Collector가 실행 중인 매체입니다." : "증거 파일을 저장하는 매체입니다."} 이미징 대상에서 제외됩니다.</p>` : ""}
      <div class="parts">${parts}</div>
    </article>`;
  }).join("");
}

function renderSelectInfo() {
  const ts = targets(), bytes = ts.reduce((a, t) => a + t.size, 0);
  $("#diskSub").textContent = state.tasks.disk ? `대상 ${ts.length}개, ${size(bytes)}` : "이번 수집에서 제외";
  $("#memSub").textContent = state.tasks.mem ? "물리 메모리 32.00 GB, 먼저 실행" : "이번 수집에서 제외";
  $("#fileList").innerHTML = ts.length
    ? ts.map((t) => `<li>${icon("file")}<span class="mono" title="${fileName(t)}">${fileName(t)}</span><span>${size(t.size)}</span></li>`).join("")
    : `<li class="empty">왼쪽에서 디스크나 볼륨을 고르세요.</li>`;
  $("#sub-select").textContent = `작업 ${taskCount()}개, ${size(neededBytes())}`;
  renderBar();
}

$("#disks").addEventListener("change", (e) => {
  const id = e.target.dataset.id;
  if (!id) return;
  if (e.target.checked) {
    state.sel.add(id);
    const d = DISKS.find((x) => x.id === id);
    if (d) d.parts.forEach((p) => state.sel.delete(p.id));
  } else state.sel.delete(id);
  if (state.sel.size && !state.tasks.disk) setTask("disk", true);
  renderDisks(); renderSelectInfo();
  const again = $(`#disks [data-id="${id}"]`);
  if (again) again.focus();
});

function setTask(k, on) {
  state.tasks[k] = on;
  const box = $(`.task[data-task="${k}"]`);
  box.querySelector("input").checked = on;
  box.classList.toggle("is-on", on);
}
$$(".task input").forEach((inp) => inp.addEventListener("change", () => { setTask(inp.closest(".task").dataset.task, inp.checked); renderSelectInfo(); }));

$$(".selector__item").forEach((b) => b.addEventListener("click", () => {
  $$(".selector__item").forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
  $("#opt-disk").hidden = b.dataset.opt !== "disk";
  $("#opt-mem").hidden = b.dataset.opt !== "mem";
}));

let refreshT;
$("#refreshDisks").onclick = () => {
  setDemo("loading");
  $("#refreshDisks").disabled = true;
  refreshT = setTimeout(() => { setDemo("full", "디스크 5개를 찾았습니다."); $("#refreshDisks").focus(); }, 900);
};
function setDemo(v, msg) {
  clearTimeout(refreshT);
  $("#refreshDisks").disabled = false;
  state.demo = v;
  $("#demoState").value = v;
  renderDisks();
  if (msg) toast(msg);
  const st = $("#navStatus");
  st.classList.toggle("is-off", v === "error");
  $("#navStatusTitle").textContent = v === "error" ? "일반 사용자 권한" : "관리자 권한";
  $("#navStatusSub").textContent = v === "error" ? "디스크와 메모리에 접근할 수 없음" : "환경 점검 5개 통과, 1개 주의";
  renderBar();
}
$("#demoState").addEventListener("change", (e) => setDemo(e.target.value));

/* ---------- 3. 수집 진행 (시뮬레이션) ---------- */
const DEMO_SEC = { mem: 7, disk: 11 };
let timer = null, simElapsed = 0;

function caseIncomplete() { return $$("#caseForm [required]").some((el) => !el.value.trim()); }

function startRun() {
  if (caseIncomplete()) {
    toast("사건 정보를 먼저 채워야 수집을 시작할 수 있습니다.");
    location.hash = "#case";
    setTimeout(submitCase, 0);
    return;
  }
  const split = Number($('input[name="split"]:checked').value);
  const algs = $$("#opt-disk .chips input").map((c, i) => c.checked && ["md5", "sha1", "sha256"][i]).filter(Boolean);
  const memAlgs = $$("#opt-mem .chips input").map((c, i) => c.checked && ["md5", "sha1", "sha256"][i]).filter(Boolean);
  state.jobs = [];
  if (state.tasks.mem) state.jobs.push({ kind: "mem", name: "메모리 덤프", target: "물리 메모리 4개 영역", size: MEM, rate: 1.4 * GiB, file: `${filePrefix()}_memory.mem`, algs: memAlgs });
  if (state.tasks.disk) targets().forEach((t) => state.jobs.push({ kind: "disk", name: `디스크 이미징: ${t.name}`, target: t.desc, size: t.size, rate: t.rate, file: fileName(t), algs, split }));
  state.jobs.forEach((j) => Object.assign(j, { read: 0, status: "wait", verify: 0, errors: 0 }));
  Object.assign(state, { started: true, cancelled: false, finished: false });
  $("#sub-case").textContent = $("#caseNo").value;
  $("#sub-done").textContent = "대기";
  simElapsed = 0;
  $("#log").innerHTML = "";
  buildJobs();
  log(`수집 시작: 사건 ${$("#caseNo").value}, 증거 ${$("#evNo").value}, 수집자 ${$("#examiner").value}`);
  log(`작업 ${state.jobs.length}개를 순서대로 실행합니다${state.tasks.mem && state.tasks.disk ? " (메모리 먼저)" : ""}`);
  $("#sub-run").textContent = "진행 중";
  $("#navStatus").classList.add("is-run");
  location.hash = "#run";
  renderRun();
  timer = setInterval(tick, 200);
}

function tick() {
  const j = state.jobs.find((x) => x.status !== "done");
  if (!j) return finish();
  if (j.status === "wait") {
    j.status = "run";
    if (j.kind === "mem") { log("드라이버 서비스 winpmem 생성 후 로드"); log("물리 메모리 영역 4개 확인, 34,359,738,368 바이트"); }
    else log(`읽기 전용으로 열기: ${j.name.split(": ")[1]}`);
  }
  if (j.status === "run") {
    const step = j.size / (DEMO_SEC[j.kind] * 5);
    j.read = Math.min(j.size, j.read + step * (0.85 + Math.random() * 0.3));
    simElapsed += step / j.rate;
    if (j.kind === "disk" && !j.warned && j.read > j.size * 0.62 && j.rate < 0.5 * GiB) { j.warned = true; j.errors = 1; log("읽기 오류 1구간 (섹터 1,882,113,024부터 8섹터), 0으로 채움", "warn"); }
    if (j.read >= j.size) {
      j.read = j.size;
      if (j.kind === "mem") { j.status = "done"; j.hashes = hashesFor(j.file); log(`메모리 덤프 완료, SHA-256 ${j.hashes.sha256.slice(0, 16)}…`, "ok"); log("드라이버 서비스 winpmem 삭제"); announce("메모리 덤프를 마쳤습니다."); }
      else { j.status = "verify"; log("읽기 완료. 이미지를 다시 읽어 해시 검증 시작"); }
    }
  } else if (j.status === "verify") {
    j.verify = Math.min(1, j.verify + 0.12);
    simElapsed += (j.size / (j.rate * 1.3)) * 0.12;
    if (j.verify >= 1) { j.status = "done"; j.hashes = hashesFor(j.file); log(`검증 일치: ${j.file}`, "ok"); announce(`${j.name}을 마쳤습니다.`); }
  }
  renderRun();
}

function finish() {
  clearInterval(timer); timer = null;
  if ($("#cancelDialog").open) $("#cancelDialog").close();
  state.finished = true;
  log("모든 작업 완료. 보고서와 흔적 기록 저장", "ok");
  announce("모든 작업을 마쳤습니다. 결과 화면에서 해시를 확인하세요.");
  $("#sub-run").textContent = `완료, ${dur(simElapsed)}`;
  $("#sub-done").textContent = "해시 검증 일치";
  $("#runLead").textContent = "모든 작업을 마쳤습니다. 결과 화면에서 해시를 확인하세요.";
  $("#navStatus").classList.remove("is-run");
  renderRun(); renderResults();
  show("run");
  if (new URLSearchParams(location.search).has("autodone")) location.hash = "#done";
}

function renderRun() {
  const total = state.jobs.reduce((a, j) => a + j.size * 2, 0);
  const done = state.jobs.reduce((a, j) => a + j.read + (j.kind === "mem" ? (j.status === "done" ? j.size : 0) : j.size * j.verify), 0);
  const pct = total ? Math.floor((done / total) * 100) : 0;
  const curIdx = state.jobs.findIndex((x) => x.status === "run" || x.status === "verify");
  $("#overallPct").textContent = `${state.finished ? 100 : pct}%`;
  $("#overallFill").style.transform = `scaleX(${state.finished ? 1 : pct / 100})`;
  $(".overall").classList.toggle("is-done", state.finished);
  $(".overall").classList.toggle("is-stop", state.cancelled);
  $("#overallSub").textContent = state.finished ? `작업 ${state.jobs.length}개 모두 완료` : state.cancelled ? "수집자가 중단했습니다" : curIdx >= 0 ? `작업 ${state.jobs.length}개 중 ${curIdx + 1}번째 진행 중` : "준비 중";
  $("#elapsed").textContent = dur(simElapsed);
  $(".overall .progress").setAttribute("aria-valuenow", String(state.finished ? 100 : pct));
  /* 작업 행은 처음에 한 번 만들고, 이후에는 바뀐 값만 갱신합니다 (진행 막대 전환 유지) */
  const rows = $$("#jobs .job");
  state.jobs.forEach((j, i) => {
    const li = rows[i];
    if (!li) return;
    const frac = j.status === "verify" ? j.verify : j.read / j.size;
    const p = Math.floor(frac * 100);
    const cls = j.status === "run" || j.status === "verify" ? "is-run" : j.status === "done" ? "is-done" : j.status === "stop" ? "is-stop" : "is-wait";
    const pill = j.status === "done" ? `<span class="pill pill--ok">${icon("check")}완료</span>`
      : j.status === "stop" ? `<span class="pill pill--stop">중단</span>`
      : j.status === "verify" ? `<span class="pill pill--run">검증 중</span>`
      : j.status === "run" ? `<span class="pill pill--run">읽는 중</span>` : `<span class="pill pill--wait">대기</span>`;
    const eta = j.status === "run" ? (j.size - j.read) / j.rate + j.size / (j.rate * 1.3) : j.status === "verify" ? (1 - j.verify) * j.size / (j.rate * 1.3) : j.status === "wait" ? j.size / j.rate * 1.77 : 0;
    const speed = j.status === "run" ? `${(j.rate / GiB * (0.95 + Math.random() * 0.1)).toFixed(2)} GB/s` : "";
    li.className = `job ${cls}`;
    const pctBox = li.querySelector(".job__pct");
    const pctHtml = j.status === "done" ? pill : `${p}%`;
    if (pctBox.innerHTML !== pctHtml) pctBox.innerHTML = pctHtml;
    const bar = li.querySelector(".progress");
    bar.setAttribute("aria-valuenow", String(j.status === "done" ? 100 : p));
    bar.firstElementChild.style.transform = `scaleX(${j.status === "done" ? 1 : frac})`;
    const sha = j.hashes && j.hashes.sha256 ? `<span>SHA-256<b class="mono"> ${j.hashes.sha256.slice(0, 20)}…</b></span>` : "";
    li.querySelector(".job__meta").innerHTML = `${j.status !== "done" ? pill : ""}${speed ? `<span>속도<b> ${speed}</b></span>` : ""}${j.status !== "done" ? `<span>남은 시간<b> ${dur(eta)}</b></span>` : sha}${j.errors ? `<span>읽기 오류<b> ${j.errors}구간</b></span>` : ""}`;
  });
  renderBar();
}

function buildJobs() {
  $("#jobs").innerHTML = state.jobs.map((j) => `<li class="job is-wait">
      <span class="job__icon">${icon(j.kind === "mem" ? "mem" : "disk", "")}</span>
      <span class="job__title"><b>${esc(j.name)}</b><small>${esc(j.target)}, ${size(j.size)}</small></span>
      <span class="job__pct">0%</span>
      <div class="progress" role="progressbar" aria-label="${esc(j.name)} 진행률" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0"><div class="progress__fill"></div></div>
      <div class="job__meta"></div>
    </li>`).join("");
}

function log(msg, kind) {
  const t = new Date(Date.UTC(2026, 10, 5, 5, 34, 50) + simElapsed * 1000 + 9 * 3600e3);
  const li = document.createElement("li");
  if (kind) li.className = "is-" + kind;
  li.innerHTML = `<time>${pad(t.getUTCHours())}:${pad(t.getUTCMinutes())}:${pad(t.getUTCSeconds())}</time><span>${esc(msg)}</span>`;
  const box = $("#log");
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 24;
  box.append(li);
  if (atBottom) box.scrollTop = box.scrollHeight;
}
const announce = (m) => { $("#announce").textContent = m; };

$("#keepBtn").onclick = () => $("#cancelDialog").close();
$("#stopBtn").onclick = () => {
  $("#cancelDialog").close();
  if (state.finished || !state.started) return;
  clearInterval(timer); timer = null;
  state.cancelled = true;
  state.jobs.forEach((j) => { if (j.status === "run" || j.status === "verify") j.status = "stop"; });
  log("수집자가 수집을 중단했습니다", "warn");
  $("#sub-run").textContent = "중단됨";
  $("#runLead").textContent = "수집을 중단했습니다. 지금까지 읽은 파일은 증거로 쓸 수 없습니다.";
  $("#navStatus").classList.remove("is-run");
  announce("수집을 중단했습니다.");
  renderRun();
  show("run");
  const r = $("#resetBtn"); if (r) r.focus();
};

/* ---------- 4. 결과 ---------- */
function hashesFor(seed) {
  let h = 2166136261;
  for (const c of seed) h = Math.imul(h ^ c.charCodeAt(0), 16777619);
  const hex = (n) => Array.from({ length: n }, () => { h = Math.imul(h ^ (h >>> 15), 2246822507); h ^= h >>> 13; return ((h >>> 0) % 16).toString(16); }).join("");
  return { md5: hex(32), sha1: hex(40), sha256: hex(64) };
}
const ALG = { md5: "MD5", sha1: "SHA-1", sha256: "SHA-256" };
function renderResults() {
  $("#results").innerHTML = state.jobs.map((j) => `
    <article class="panel result">
      <div class="result__head">
        <span class="result__icon">${icon(j.kind === "mem" ? "mem" : "disk", "")}</span>
        <span class="result__name"><b>${esc(j.file)}</b><small>${size(j.size)}${j.kind === "disk" ? (j.split ? `, ${j.split} GB 단위로 ${Math.ceil(j.size / (j.split * GiB))}개 분할` : ", 분할 없음") : ""}${j.errors ? `, 읽기 오류 ${j.errors}구간` : ""}</small></span>
        <span class="pill pill--ok">${icon("check")}검증 일치</span>
      </div>
      <dl class="hashes">${(j.algs.length ? j.algs : []).map((k) => `<div><dt>${ALG[k]}</dt><dd>${j.hashes[k]}</dd><button type="button" class="icon-btn" data-copy="${j.hashes[k]}" aria-label="${ALG[k]} 복사">${icon("copy", "")}</button></div>`).join("")}</dl>
    </article>`).join("");
}
$("#results").addEventListener("click", (e) => {
  const b = e.target.closest("[data-copy]");
  if (!b) return;
  navigator.clipboard?.writeText(b.dataset.copy).catch(() => {});
  toast("해시를 복사했습니다.");
});
$("#openFolder").onclick = () => toast(`${evidenceDir()} 폴더를 열었습니다.`);
$("#exportReport").onclick = () => toast(`보고서를 저장했습니다: ${val("evNo")}_report.pdf`);

function resetAll() {
  clearInterval(timer); timer = null;
  Object.assign(state, { started: false, finished: false, cancelled: false, jobs: [] });
  simElapsed = 0;
  $("#jobs").innerHTML = ""; $("#log").innerHTML = ""; $("#results").innerHTML = "";
  renderRun();
  $("#sub-run").textContent = "대기"; $("#sub-done").textContent = "대기";
  $("#runLead").textContent = "창을 닫거나 USB를 뽑지 마세요. 증거 파일이 손상될 수 있습니다.";
  /* 같은 사건의 다음 증거를 바로 수집할 수 있게 수집자, 소속, 저장 폴더, 사건 번호는 남기고 증거 번호와 메모만 비웁니다 */
  $("#evNo").value = ""; $("#memo").value = "";
  $("#sub-case").textContent = "입력 중"; $("#sub-select").textContent = "대상과 옵션 선택";
  renderPathPreview();
  location.hash = "#case";
  show("case");
  $("#evNo").focus();
  toast("새 수집을 시작합니다. 증거 번호를 입력하세요.");
}

/* ---------- 알림 ---------- */
let toastT;
function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("is-on");
  clearTimeout(toastT);
  toastT = setTimeout(() => t.classList.remove("is-on"), 2600);
}

/* 본문 건너뛰기: 해시 라우팅과 겹치지 않도록 직접 포커스 이동 */
$('.skip').addEventListener('click', (e) => { e.preventDefault(); $('#main').focus(); });

/* ---------- 시작 ---------- */
renderDisks();
renderSelectInfo();
renderPathPreview();
show(location.hash.slice(1) || "case", { initial: true });
/* 스크린샷 확인용: ?autorun 이면 바로 수집을 시작합니다 */
if (new URLSearchParams(location.search).has("autorun")) {
  $("#caseNo").value = "2026-형제-01187"; $("#evNo").value = "EV-003"; $("#examiner").value = "홍길동"; $("#outPath").value = "E:\\Cases";
  renderPathPreview(); startRun();
}
