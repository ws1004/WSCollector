// 화면 상태 (zustand). 저장은 메모리에만 합니다 (AppData, localStorage 사용 금지: 흔적 원칙)
import { create } from "zustand";
import { api } from "./api";
import type {
  CaseInfo, CollectEvent, CollectSummary, Compression, DiskInfo, Environment, ImageFormat, FinishEvent, HashAlg, JobOutcome, JobPlan, LogEntry, OutputCheck, Progress,
  ReceiveFinish, ReceiveOutcome, RecvEvent, Target,
} from "./types";

export type Screen = "case" | "select" | "run" | "done";
/** 수집(이 PC가 대상) / 네트워크 수신(이 PC가 분석 PC) */
export type Mode = "collect" | "receive";
/** 증거 저장 위치: 이 PC에 연결한 매체 / 네트워크 수신 PC */
export type Dest = "local" | "net";
export type DisksState = "loading" | "ready" | "error";

export interface CaseForm {
  caseNo: string;
  evNo: string;
  examiner: string;
  org: string;
  outBase: string;
  notes: string;
  /** 네트워크 수신 PC 주소 (예: 192.168.0.10 또는 192.168.0.10:7700) */
  netAddr: string;
  /** 수신 PC 화면의 연결 코드 */
  netCode: string;
}

export interface RxFile {
  name: string;
  size: number;
  state: "recv" | "saved" | "incomplete";
}

export type RxPhase = "idle" | "starting" | "listening" | "receiving" | "done" | "stopped" | "failed";

export interface RxState {
  outBase: string;
  port: string;
  /** 받을 네트워크 (이 PC 주소). 빈 문자열 = 모든 네트워크 */
  bind: string;
  outCheck: OutputCheck | null;
  outChecking: boolean;
  error: string | null;
  phase: RxPhase;
  code: string;
  addrs: string[];
  peer: string;
  host: string;
  case: CaseInfo | null;
  evidenceDir: string;
  need: number;
  total: number;
  rate: number;
  verify: { done: number; total: number } | null;
  files: RxFile[];
  events: LogEntry[];
  outcome: ReceiveOutcome | null;
  message: string | null;
  stopOpen: boolean;
  /** 연결이 끊겨 이어받기를 기다리는 중이면 기다림이 끝나는 시각(ms) */
  waitUntil: number | null;
  resumes: number;
}

export interface JobRun {
  plan: JobPlan;
  status: "wait" | "run" | "done" | "failed" | "cancelled" | "skipped";
  progress: Progress | null;
  outcome: JobOutcome | null;
}

interface State {
  mode: Mode;
  dest: Dest;
  rx: RxState;
  /** 진행 중, 끝난 수집이 네트워크로 보낸 것이면 수신 PC 주소 */
  runRemote: string | null;
  screen: Screen;
  env: Environment | null;
  /** env를 받은 시각 (대상 시스템 시계 표시 기준) */
  envAt: number;
  envError: string | null;
  form: CaseForm;
  errors: Partial<Record<keyof CaseForm, string>>;
  outCheck: OutputCheck | null;
  outChecking: boolean;

  disks: DiskInfo[];
  disksState: DisksState;
  disksError: string | null;
  /** "d:0" = Disk 0 전체, "p:0:3" = Disk 0 파티션 3 */
  selection: string[];
  taskDisk: boolean;
  /** 메모리 수집 (Linux) */
  taskMem: boolean;
  optTab: "disk" | "mem";
  splitGiB: 0 | 2 | 4;
  format: ImageFormat;
  compression: Compression;
  algs: Record<HashAlg, boolean>;
  verify: boolean;
  zeroFill: boolean;

  starting: boolean;
  started: boolean;
  finished: boolean;
  cancelled: boolean;
  startedAt: number;
  evidenceDir: string;
  jobs: JobRun[];
  logs: LogEntry[];
  summary: CollectSummary | null;
  failMessage: string | null;

  toast: { id: number; msg: string } | null;
  cancelOpen: boolean;
  /** 화면 전환 뒤 포커스를 줄 입력칸 id (없으면 제목) */
  focusAfter: string | null;
}

const initialForm: CaseForm = { caseNo: "", evNo: "", examiner: "", org: "", outBase: "", notes: "", netAddr: "", netCode: "" };

const initialRx: RxState = {
  outBase: "",
  port: "7700",
  bind: "",
  outCheck: null,
  outChecking: false,
  error: null,
  phase: "idle",
  code: "",
  addrs: [],
  peer: "",
  host: "",
  case: null,
  evidenceDir: "",
  need: 0,
  total: 0,
  rate: 0,
  verify: null,
  files: [],
  events: [],
  outcome: null,
  message: null,
  stopOpen: false,
  waitUntil: null,
  resumes: 0,
};

export const useApp = create<State>(() => ({
  mode: "collect",
  dest: "local",
  rx: initialRx,
  runRemote: null,
  screen: "case",
  env: null,
  envAt: 0,
  envError: null,
  form: initialForm,
  errors: {},
  outCheck: null,
  outChecking: false,
  disks: [],
  disksState: "loading",
  disksError: null,
  selection: [],
  taskDisk: true,
  taskMem: false,
  optTab: "disk",
  splitGiB: 0,
  format: "e01",
  compression: "best",
  algs: { md5: true, sha1: true, sha256: true },
  verify: true,
  zeroFill: true,
  starting: false,
  started: false,
  finished: false,
  cancelled: false,
  startedAt: 0,
  evidenceDir: "",
  jobs: [],
  logs: [],
  summary: null,
  failMessage: null,
  toast: null,
  cancelOpen: false,
  focusAfter: null,
}));

const set = useApp.setState;
const get = useApp.getState;

// -- 공통 ------------------------------------------------
/** 실행 중인 운영체제 (환경 정보가 오기 전에는 Windows로 가정) */
export const isLinux = (s: State = get()) => s.env?.os === "linux";
/** 경로 구분자 */
export const sepOf = (s: State = get()) => (isLinux(s) ? "/" : "\\");
/** 저장 폴더 예시 */
export const outExample = (s: State = get()) => (isLinux(s) ? "/mnt/usb/Cases" : "E:\\Cases");
/** 전체 경로 형식인지 (Windows: 드라이브 문자, Linux: /로 시작) */
export const isAbsPath = (p: string, s: State = get()) => (isLinux(s) ? p.startsWith("/") : /^[A-Za-z]:\\/.test(p));
const absPathMsg = (s: State = get()) =>
  isLinux(s) ? `/로 시작하는 전체 경로를 입력하세요. 예: ${outExample(s)}` : `드라이브 문자로 시작하는 전체 경로를 입력하세요. 예: ${outExample(s)}`;

let toastSeq = 0;
export function toast(msg: string) {
  set({ toast: { id: ++toastSeq, msg } });
}

export function go(screen: Screen) {
  const s = get();
  if (s.mode === "receive") {
    if (rxActive(s)) return;
    set({ mode: "collect" });
  }
  if (screen === "run" && !s.started) return;
  if (screen === "done" && !s.finished) return;
  if (s.started && !s.finished && (screen === "case" || screen === "select")) return;
  set({ screen });
}

export async function loadEnvironment() {
  try {
    const env = await api.environment();
    set({ env, envAt: Date.now(), envError: null });
  } catch (e) {
    set({ envError: String(e) });
  }
}

// -- 사건 정보 ------------------------------------------
const BAD_CHARS = /[\\/:*?"<>|]/;
const LABELS: Record<keyof CaseForm, string> = {
  caseNo: "사건 번호", evNo: "증거 번호", examiner: "수집자", org: "소속", outBase: "저장 폴더", notes: "현장 메모",
  netAddr: "수신 PC 주소", netCode: "연결 코드",
};

/** 연결 코드: 하이픈, 공백 무시, 대문자. 헷갈리는 0, 1, I, O는 쓰지 않음 */
export const normCode = (v: string) => v.replace(/[\s-]/g, "").toUpperCase();
const CODE_RE = /^[2-9A-HJ-NP-Z]{10}$/;

export function fieldError(k: keyof CaseForm, v: string): string {
  const t = v.trim();
  if (k === "org" || k === "notes") return "";
  if (!t) return `${LABELS[k]} 항목을 입력해야 보고서를 만들 수 있습니다.`;
  if ((k === "caseNo" || k === "evNo") && BAD_CHARS.test(t)) return '폴더 이름에 쓸 수 없는 문자(\\ / : * ? " < > |)가 들어 있습니다.';
  if ((k === "caseNo" || k === "evNo") && /\.$/.test(t)) return "점(.)으로 끝나는 이름은 폴더 이름으로 쓸 수 없습니다.";
  if (k === "outBase" && !isAbsPath(t)) return absPathMsg();
  if (k === "netAddr") {
    if (/\s/.test(t) || !/^[\w.\-:[\]]+$/.test(t)) return "IP 주소나 호스트 이름을 입력하세요. 예: 192.168.0.10";
    // 포트: "호스트:포트" 또는 "[IPv6]:포트". 콜론이 여럿이고 대괄호가 없으면 IPv6 주소만 쓴 것
    const port = t.startsWith("[") ? /\]:(.*)$/.exec(t)?.[1] : t.split(":").length === 2 ? t.split(":")[1] : undefined;
    if (port !== undefined && (!/^\d{1,5}$/.test(port) || Number(port) < 1 || Number(port) > 65535)) return "포트는 1~65535 사이 숫자여야 합니다. 예: 192.168.0.10:7700";
  }
  if (k === "netCode" && !CODE_RE.test(normCode(t))) return "수신 PC 화면의 10자리 코드를 입력하세요. 예: 4KQ7M-2XD9P";
  return "";
}

export function setField(k: keyof CaseForm, v: string) {
  const s = get();
  const errors = { ...s.errors };
  if (errors[k] && !fieldError(k, v)) delete errors[k];
  set({ form: { ...s.form, [k]: v }, errors });
  if (k === "outBase") set({ outCheck: null });
}

let outSeq = 0;
export async function checkOutput() {
  if (get().dest === "net") return;
  const p = get().form.outBase.trim();
  if (!p || fieldError("outBase", p)) {
    set({ outCheck: null });
    return;
  }
  const my = ++outSeq;
  set({ outChecking: true });
  try {
    const r = await api.checkOutput(p);
    if (my === outSeq) {
      set({ outCheck: r });
      // FAT32는 4GB 넘는 파일을 못 만듦: RAW는 2GB 분할로 고정, E01은 엔진이 알아서 나눔(4GB 조각은 불가)
      if (r.volume?.fs.toUpperCase() === "FAT32") {
        const g = get();
        if (g.format === "raw" && g.splitGiB !== 2) set({ splitGiB: 2 });
        if (g.format === "e01" && g.splitGiB === 4) set({ splitGiB: 0 });
      }
    }
  } catch (e) {
    if (my === outSeq) set({ outCheck: { ok: false, message: String(e), volume: null, diskModel: null, diskNumber: null, warning: null } });
  } finally {
    if (my === outSeq) set({ outChecking: false });
  }
}

export async function pickFolder() {
  const p = await api.pickFolder();
  if (!p) return;
  setField("outBase", p);
  await checkOutput();
}

/** 다음 단계로: 모든 필수 항목 검사. 실패한 첫 항목 id를 돌려줌 */
export async function submitCase(): Promise<keyof CaseForm | null> {
  const s = get();
  const errors: State["errors"] = {};
  const keys: (keyof CaseForm)[] = ["caseNo", "evNo", "examiner", ...(s.dest === "net" ? (["netAddr", "netCode"] as const) : (["outBase"] as const))];
  keys.forEach((k) => {
    const e = fieldError(k, s.form[k]);
    if (e) errors[k] = e;
  });
  if (s.dest === "local" && !errors.outBase) {
    await checkOutput();
    const oc = get().outCheck;
    if (oc && !oc.ok) errors.outBase = oc.message;
  }
  set({ errors });
  const first = keys.find((k) => errors[k]);
  if (first) return first;
  set({ screen: "select" });
  void loadDisks();
  return null;
}

export function evidenceDir(f: CaseForm = get().form): string {
  const sp = sepOf();
  return `${f.outBase.trim().replace(/[\\/]+$/, "")}${sp}${f.caseNo.trim()}${sp}${f.evNo.trim()}`;
}

// -- 수집 항목 ------------------------------------------
let diskSeq = 0;
export async function loadDisks() {
  const my = ++diskSeq;
  set({ disksState: "loading", disksError: null });
  try {
    const disks = await api.listDisks(get().dest === "net" ? null : get().form.outBase.trim() || null);
    if (my !== diskSeq) return;
    // 선택 불가가 된 항목(저장 폴더가 있는 디스크 전체, 파티션)의 선택은 지움
    const st = get();
    const selection = st.selection.filter((k) => {
      const [kind, dn, pn] = k.split(":");
      const d = disks.find((x) => x.number === Number(dn));
      if (!d) return false;
      if (kind === "d") return !isLocked(d);
      const p = d.partitions.find((x) => x.number === Number(pn));
      return !!p && !isOutputPart(p, st);
    });
    set({ disks, disksState: "ready", selection });
  } catch (e) {
    if (my === diskSeq) set({ disksState: "error", disksError: String(e) });
  }
}

/** 디스크 전체 선택 불가: 저장 폴더가 이 디스크에 있음 (이미징하는 동안 이미지가 대상 안에 써짐). 도구 디스크는 막지 않음 */
export const isLocked = (d: DiskInfo) => d.roles.includes("output");
/** 저장 폴더가 있는 바로 그 파티션(볼륨)인지. 같은 디스크의 다른 파티션은 고를 수 있음 */
export const isOutputPart = (p: { letter: string | null }, s: State = get()) => {
  const l = s.dest === "local" ? s.outCheck?.volume?.letter : null;
  return !!l && !!p.letter && p.letter.toUpperCase() === l.toUpperCase();
};
export const isExternal = (d: DiskInfo) => d.removable || ["USB", "IEEE 1394", "SD", "MMC"].includes(d.bus);
export const kindLabel = (d: DiskInfo) => (d.removable ? "이동식" : isExternal(d) ? "외장" : "내장");

export function toggleSelect(key: string, on: boolean) {
  const s = get();
  let sel = s.selection.filter((k) => k !== key);
  if (on) {
    sel.push(key);
    if (key.startsWith("d:")) {
      const n = key.slice(2);
      sel = sel.filter((k) => !k.startsWith(`p:${n}:`));
    }
  }
  set({ selection: sel, taskDisk: sel.length > 0 ? true : s.taskDisk });
}

export interface SelectedTarget {
  key: string;
  target: Target;
  title: string;
  desc: string;
  size: number;
  tag: string;
}

/** Linux 장치 경로의 파일 이름 꼬리표 (/dev/sda → sda, /dev/mapper/vg-root → vg-root) */
const devTag = (path: string) => path.replace(/^\/dev\//, "").replace(/^mapper\//, "").replace(/[^A-Za-z0-9_-]/g, "_");

export function selectedTargets(s: State = get()): SelectedTarget[] {
  const out: SelectedTarget[] = [];
  const linux = isLinux(s);
  for (const d of s.disks) {
    if (s.selection.includes(`d:${d.number}`)) {
      if (linux) {
        out.push({ key: `d:${d.number}`, target: { type: "device", path: d.path, serial: d.serial || undefined }, title: d.path, desc: d.model, size: Number(d.size), tag: devTag(d.path) });
      } else {
        out.push({ key: `d:${d.number}`, target: { type: "disk", number: d.number, serial: d.serial || undefined }, title: `Disk ${d.number}`, desc: d.model, size: Number(d.size), tag: `Disk${d.number}` });
      }
      continue;
    }
    for (const p of d.partitions) {
      const key = `p:${d.number}:${p.number}`;
      if (!s.selection.includes(key)) continue;
      if (linux && p.device) {
        out.push({ key, target: { type: "device", path: p.device }, title: p.device, desc: `${d.path}${p.letter ? `, ${p.letter}` : ""}`, size: Number(p.size), tag: devTag(p.device) });
      } else if (p.letter && !linux) {
        out.push({ key, target: { type: "volume", letter: p.letter }, title: partName(p), desc: `Disk ${d.number}`, size: Number(p.size), tag: p.letter.replace(":", "") });
      } else {
        out.push({ key, target: { type: "partition", disk: d.number, number: p.number, serial: d.serial || undefined }, title: partName(p), desc: `Disk ${d.number}`, size: Number(p.size), tag: `Disk${d.number}_P${p.number}` });
      }
    }
  }
  return out;
}

export function partName(p: { letter: string | null; label: string; kind: string; number: number }) {
  if (p.letter) return p.label ? `${p.letter} ${p.label}` : p.letter;
  return p.kind || `파티션 ${p.number}`;
}

export function filePrefix(s: State = get()): string {
  const host = s.env?.host.hostname ?? "HOST";
  const date = (s.env?.host.localTime ?? "").slice(0, 10).replace(/-/g, "");
  return `${s.form.evNo.trim()}_${host}_${date}`;
}

export function neededBytes(s: State = get()): number {
  const cap = s.env?.testMaxBytes ? Number(s.env.testMaxBytes) : Infinity;
  const disks = s.taskDisk ? selectedTargets(s).reduce((a, t) => a + Math.min(t.size, cap), 0) : 0;
  const mem = s.taskMem && s.env?.memory ? Math.min(Number(s.env.memory.imageBytes), cap) : 0;
  return disks + mem;
}

/** 이번 수집의 작업 수 (메모리 1 + 디스크 대상) */
export function jobCount(s: State = get()): number {
  return (s.taskMem && s.env?.memory ? 1 : 0) + (s.taskDisk ? selectedTargets(s).length : 0);
}

export const isFat32 = (s: State = get()) => s.outCheck?.volume?.fs.toUpperCase() === "FAT32";

/** 결과 요약: 모두 검증 일치 / 검증 안 함 / 읽기 오류 / 실패 */
export function resultKind(sm: CollectSummary): "verified" | "unverified" | "badsectors" | "partial" {
  if (sm.status !== "completed") return "partial";
  if (sm.jobs.some((j) => (j.result?.badSectors ?? 0) > 0)) return "badsectors";
  if (sm.jobs.every((j) => j.result?.verify?.matched)) return "verified";
  return "unverified";
}

export function freeBytes(s: State = get()): number | null {
  if (s.dest === "net") return null;
  return s.outCheck?.volume ? Number(s.outCheck.volume.free) : null;
}

export function setDest(d: Dest) {
  const s = get();
  if (s.dest === d) return;
  // 바뀐 쪽 항목의 오류 표시만 지움
  const { outBase: _o, netAddr: _a, netCode: _c, ...rest } = s.errors;
  set({ dest: d, errors: rest, focusAfter: null });
  if (d === "local") void checkOutput();
}

// -- 수집 실행 ------------------------------------------
export async function startCollection() {
  const s = get();
  const targets: Target[] = [
    ...(s.taskMem && s.env?.memory ? [{ type: "memory" } as Target] : []),
    ...(s.taskDisk ? selectedTargets(s).map((t) => t.target) : []),
  ];
  const algs = (Object.keys(s.algs) as HashAlg[]).filter((k) => s.algs[k]);
  if (targets.length === 0) return toast(s.env?.memory ? "메모리 수집을 켜거나 이미징할 디스크를 고르세요." : "이미징할 디스크나 볼륨을 고르세요.");
  if (algs.length === 0) return toast("해시를 하나 이상 고르세요.");
  const remote = s.dest === "net" ? { addr: s.form.netAddr.trim(), code: normCode(s.form.netCode) } : null;
  set({ starting: true });
  try {
    set({ jobs: [], logs: [], summary: null, failMessage: null, evidenceDir: "", cancelled: false, finished: false, runRemote: remote?.addr ?? null });
    await api.start({
      case: { caseNumber: s.form.caseNo.trim(), evidenceNumber: s.form.evNo.trim(), examiner: s.form.examiner.trim(), organization: s.form.org.trim(), notes: s.form.notes.trim() },
      outBase: remote ? "" : s.form.outBase.trim(),
      remote,
      targets,
      splitBytes: s.splitGiB * 1024 ** 3,
      format: s.format,
      compression: s.compression,
      algs,
      verify: s.verify,
      zeroFillErrors: s.zeroFill,
      interface: "GUI",
    });
    set({ started: true, startedAt: Date.now(), screen: "run" });
  } catch (e) {
    set({ runRemote: null });
    toast(String(e));
  } finally {
    set({ starting: false });
  }
}

export function onCollect(e: CollectEvent) {
  const s = get();
  switch (e.kind) {
    case "prepared":
      set({ evidenceDir: e.evidenceDir, jobs: e.jobs.map((plan) => ({ plan, status: "wait", progress: null, outcome: null })) });
      break;
    case "jobStarted":
      set({ jobs: s.jobs.map((j, i) => (i === e.index ? { ...j, status: "run" } : j)) });
      break;
    case "progress":
      set({ jobs: s.jobs.map((j, i) => (i === e.index ? { ...j, progress: e.progress } : j)) });
      break;
    case "jobFinished":
      set({ jobs: s.jobs.map((j, i) => (i === e.index ? { ...j, status: e.outcome.status, outcome: e.outcome } : j)) });
      break;
    case "log": {
      const { kind: _k, ...entry } = e;
      set({ logs: [...s.logs, entry] });
      break;
    }
  }
}

export function onFinish(e: FinishEvent) {
  if (e.kind === "done") {
    const cancelled = e.summary.status === "cancelled";
    set({
      finished: !cancelled,
      cancelled,
      summary: e.summary,
      cancelOpen: false,
      jobs: get().jobs.map((j, i) => {
        const o = e.summary.jobs[i];
        return o ? { ...j, status: o.status, outcome: o } : j;
      }),
    });
  } else {
    set({ cancelled: true, failMessage: e.message, cancelOpen: false });
    toast(`수집 실패: ${e.message}`);
  }
}

export async function confirmCancel() {
  set({ cancelOpen: false });
  if (!get().started || get().finished) return;
  const ok = await api.cancel();
  toast(ok ? "중단 요청을 보냈습니다. 현재 작업을 정리하는 중입니다…" : "이미 끝난 수집입니다.");
}

export function newCollection() {
  const s = get();
  set({
    screen: "case",
    // 연결 코드는 수신 세션마다 새로 만들어지므로 비움
    form: { ...s.form, evNo: "", notes: "", netCode: "" },
    errors: {},
    started: false,
    finished: false,
    cancelled: false,
    jobs: [],
    logs: [],
    summary: null,
    failMessage: null,
    evidenceDir: "",
    selection: [],
    focusAfter: "evNo",
  });
  toast("새 수집을 시작합니다. 증거 번호를 입력하세요.");
}

// -- 네트워크 수신 --------------------------------------
export const rxActive = (s: State = get()) => ["starting", "listening", "receiving"].includes(s.rx.phase);

function rx(patch: Partial<RxState>) {
  set({ rx: { ...get().rx, ...patch } });
}

function nowTime() {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 수신 기록은 최근 500줄만 (인증 전 접속이 반복돼도 화면이 느려지지 않게) */
const RX_LOG_MAX = 500;
function rxLog(level: LogEntry["level"], message: string) {
  const ev = get().rx.events;
  const next = ev.length >= RX_LOG_MAX ? ev.slice(ev.length - RX_LOG_MAX + 1) : ev.slice();
  next.push({ time: nowTime(), level, message });
  rx({ events: next });
}

export function openReceive() {
  const s = get();
  if (s.starting || (s.started && !s.finished && !s.cancelled)) return toast("수집 중에는 수신 화면으로 바꿀 수 없습니다.");
  set({ mode: "receive", focusAfter: null });
}

export function setRxField(k: "outBase" | "port" | "bind", v: string) {
  rx({ [k]: v, ...(k === "outBase" ? { outCheck: null } : {}), error: null } as Partial<RxState>);
}

let rxSeq = 0;
export async function checkRxOutput() {
  const p = get().rx.outBase.trim();
  if (!p) return rx({ outCheck: null });
  if (!isAbsPath(p)) return rx({ outCheck: { ok: false, message: absPathMsg(), volume: null, diskModel: null, diskNumber: null, warning: null } });
  const my = ++rxSeq;
  rx({ outChecking: true });
  try {
    const r = await api.checkOutput(p);
    if (my === rxSeq) rx({ outCheck: r });
  } catch (e) {
    if (my === rxSeq) rx({ outCheck: { ok: false, message: String(e), volume: null, diskModel: null, diskNumber: null, warning: null } });
  } finally {
    if (my === rxSeq) rx({ outChecking: false });
  }
}

export async function pickRxFolder() {
  const p = await api.pickFolder();
  if (!p) return;
  setRxField("outBase", p);
  await checkRxOutput();
}

export async function startReceive() {
  const r = get().rx;
  // 두 번 눌러도 한 번만: 검사 전에 바로 "준비 중"으로 바꿈
  if (r.phase !== "idle") return;
  const port = Number(r.port);
  if (!r.outBase.trim()) return rx({ error: "받은 증거를 저장할 폴더를 고르세요." });
  if (!Number.isInteger(port) || port < 1 || port > 65535) return rx({ error: "포트는 1~65535 사이 숫자여야 합니다." });
  rx({ phase: "starting", error: null });
  await checkRxOutput();
  const oc = get().rx.outCheck;
  if (oc && !oc.ok) return rx({ phase: "idle", error: oc.message });
  rx({ ...initialRx, outBase: r.outBase, port: r.port, bind: r.bind, outCheck: oc, phase: "starting" });
  try {
    await api.startReceive(r.outBase.trim(), port, r.bind || null);
  } catch (e) {
    // 백엔드가 이미 시작했다가 끝난 경우(receive-finish)와 겹치지 않게 아직 준비 중일 때만 되돌림
    if (get().rx.phase === "starting") rx({ phase: "idle", error: String(e) });
    else toast(String(e));
  }
}

export async function confirmStopReceive() {
  rx({ stopOpen: false });
  if (!rxActive()) return;
  try {
    if (!(await api.stopReceive())) toast("이미 끝난 수신입니다.");
  } catch (e) {
    toast(String(e));
  }
}

export function askStopReceive() {
  set((s) => ({ rx: { ...s.rx, stopOpen: true } }));
}

export function resetReceive() {
  const r = get().rx;
  rx({ ...initialRx, outBase: r.outBase, port: r.port, outCheck: r.outCheck });
}

export function onReceive(e: RecvEvent) {
  const r = get().rx;
  switch (e.kind) {
    case "listening":
      rx({ phase: "listening", code: e.code, addrs: e.addrs });
      rxLog("info", `접속 대기 시작: ${e.addrs.join(", ") || "주소 확인 안 됨"}`);
      break;
    case "authFailed":
      rxLog("warn", `${e.peer}: 연결 코드가 맞지 않습니다 (${e.count}번째). 다음 접속까지 ${e.delaySecs}초 기다립니다`);
      break;
    case "rejected":
      rxLog("warn", `${e.peer}: 접속 거부 (${e.reason})`);
      break;
    case "accepted":
      // 대기 중에 연 중지 대화상자는 닫음 (문구가 바뀐 채 눌리지 않게)
      rx({ phase: "receiving", peer: e.peer, case: e.case, host: e.host, evidenceDir: e.evidenceDir, need: e.needBytes, stopOpen: false });
      rxLog("info", `접속: ${e.host} (${e.peer}), 사건 ${e.case.caseNumber} / 증거 ${e.case.evidenceNumber} / 수집자 ${e.case.examiner}`);
      break;
    case "fileOpened":
      rx({ files: [...r.files, { name: e.name, size: 0, state: "recv" }] });
      break;
    case "fileClosed": {
      const files = r.files.map((f): RxFile => (f.name === e.name ? { ...f, size: e.size, state: "saved" } : f));
      // 진행 알림(0.5초 간격)보다 빨리 끝난 전송도 받은 양이 맞게 보이도록 저장된 크기로 보정
      const saved = files.reduce((a, f) => a + (f.state === "saved" ? f.size : 0), 0);
      rx({ files, total: Math.max(r.total, saved) });
      rxLog("info", `저장: ${e.name}`);
      break;
    }
    case "fileIncomplete":
      rx({ files: [...r.files.filter((f) => f.name !== e.name && `${f.name}.incomplete` !== e.name), { name: e.name, size: e.size, state: "incomplete" }] });
      rxLog("warn", `끝까지 받지 못한 파일: ${e.name}`);
      break;
    case "received":
      rx({ total: e.total, rate: e.rateBps, verify: null });
      break;
    case "verifying":
      rx({ verify: { done: e.done, total: e.total } });
      break;
    case "verifyDone":
      rx({ verify: null });
      rxLog("info", "검증용 다시 읽기 완료 (해시 비교 결과는 보내는 쪽 화면과 보고서에 있음)");
      break;
    case "finished":
      rxLog("info", `보내는 쪽 세션 종료: ${e.status}`);
      break;
    case "disconnected":
      rx({ waitUntil: Date.now() + e.waitSecs * 1000, rate: 0 });
      rxLog("warn", `연결이 끊어졌습니다 (${e.reason}). 보내는 쪽이 다시 연결해 이어받기를 ${Math.round(e.waitSecs / 60)}분 동안 기다립니다`);
      break;
    case "resumed":
      rx({ waitUntil: null, resumes: e.count });
      rxLog("info", `다시 연결됨: ${e.peer} (이어받기 ${e.count}번째)`);
      break;
  }
}

export function onReceiveFinish(e: ReceiveFinish) {
  if (e.kind === "done") {
    rx({ phase: "done", outcome: e.outcome, stopOpen: false, verify: null, waitUntil: null });
    toast("수신을 마쳤습니다.");
  } else if (e.kind === "stopped") {
    rx({ phase: get().rx.phase === "starting" || get().rx.phase === "listening" ? "idle" : "stopped", stopOpen: false, verify: null, waitUntil: null });
    rxLog("warn", "수신을 중지했습니다");
  } else {
    const wasIdle = get().rx.phase === "starting";
    rx({ phase: wasIdle ? "idle" : "failed", message: e.message, error: wasIdle ? e.message : null, stopOpen: false, verify: null, waitUntil: null });
    rxLog("error", e.message);
  }
}
