// ws-core 데이터 구조 (Rust serde camelCase와 일치). u64는 문자열로 옵니다.

export type HashAlg = "md5" | "sha1" | "sha256";

export interface HostInfo {
  hostname: string;
  osName: string;
  osVersion: string;
  osBuild: string;
  arch: string;
  cpu: string;
  ramBytes: string;
  manufacturer: string;
  model: string;
  bootTime: string;
  localTime: string;
  timezone: string;
}

export interface Check {
  id: string;
  title: string;
  detail: string;
  status: "pass" | "warn" | "fail" | "pending";
}

export interface MemoryInfo {
  bytes: string;
  imageBytes: string;
  source: string;
  ranges: number;
  notes: string[];
  /** Windows: 적재할 WinPmem 드라이버 파일 경로 (Linux는 없음) */
  driver?: string;
}

export interface Environment {
  version: string;
  os: "windows" | "linux";
  host: HostInfo;
  memory: MemoryInfo | null;
  /** 메모리를 수집할 수 없는 이유 (Linux) */
  memoryError?: string | null;
  /** 이 PC의 IPv4 주소들 (네트워크 수신 "받을 네트워크") */
  localIps?: string[];
  elevated: boolean;
  exeDir: string;
  toolMedia: string | null;
  checks: Check[];
  testMaxBytes: string | null;
}

export interface PartitionInfo {
  number: number;
  offset: string;
  size: string;
  kind: string;
  name: string;
  letter: string | null;
  /** Linux: 파티션 장치 경로 */
  device?: string | null;
  label: string;
  fs: string;
  free: string | null;
}

export type DiskRole = "tool" | "output" | "system";

export interface DiskInfo {
  number: number;
  path: string;
  model: string;
  serial: string;
  bus: string;
  removable: boolean;
  size: string;
  sectorSize: number;
  partitionStyle: string;
  partitions: PartitionInfo[];
  roles: DiskRole[];
}

export interface VolumeInfo {
  letter: string;
  label: string;
  fs: string;
  size: string;
  free: string;
  diskNumber: number | null;
  driveType: string;
}

export interface OutputCheck {
  ok: boolean;
  message: string;
  volume: VolumeInfo | null;
  diskModel: string | null;
  diskNumber: number | null;
  warning: string | null;
}

export type Target =
  | { type: "disk"; number: number; serial?: string }
  | { type: "volume"; letter: string }
  | { type: "partition"; disk: number; number: number; serial?: string }
  | { type: "device"; path: string; serial?: string }
  | { type: "memory" };

export interface CaseInfo {
  caseNumber: string;
  evidenceNumber: string;
  examiner: string;
  organization: string;
  notes: string;
}

export interface RemoteTarget {
  /** "192.168.0.10" 또는 "192.168.0.10:7700" */
  addr: string;
  code: string;
}

export type ImageFormat = "e01" | "raw";
export type Compression = "best" | "fast" | "none";

export interface CollectRequest {
  case: CaseInfo;
  outBase: string;
  /** 네트워크 전송: 있으면 outBase 대신 수신 PC에 저장 */
  remote?: RemoteTarget | null;
  targets: Target[];
  /** RAW: 분할 크기, E01: 조각 크기. 0 = 나누지 않음 */
  splitBytes: number;
  format: ImageFormat;
  compression: Compression;
  algs: HashAlg[];
  verify: boolean;
  zeroFillErrors: boolean;
  interface: string;
}

export interface Progress {
  phase: "reading" | "verifying";
  done: number;
  total: number;
  rateBps: number;
  etaSecs: number;
  badSectors: number;
}

export interface OutputFile {
  name: string;
  size: number;
}

export interface ImageResult {
  files: OutputFile[];
  /** 원본에서 읽은 크기 (E01은 풀었을 때 크기) */
  totalBytes: number;
  /** 저장한 파일 크기 합 (E01은 압축 후) */
  storedBytes: number;
  format: ImageFormat;
  sectorSize: number;
  hashes: Partial<Record<HashAlg, string>>;
  verify: { hashes: Partial<Record<HashAlg, string>>; matched: boolean; secs: number; error: string | null } | null;
  badRanges: { startSector: number; count: number }[];
  badSectors: number;
  cancelled: boolean;
  readSecs: number;
}

export type JobStatus = "done" | "failed" | "cancelled" | "skipped";

export interface JobOutcome {
  index: number;
  title: string;
  description: string;
  status: JobStatus;
  result: ImageResult | null;
  error: string | null;
}

export interface JobPlan {
  title: string;
  description: string;
  size: string;
  fileBase: string;
}

export interface LogEntry {
  time: string;
  level: "info" | "warn" | "error";
  message: string;
}

export interface ProcInfo {
  pid: number;
  parentPid: number;
  name: string;
}

export interface CollectSummary {
  toolVersion: string;
  case: CaseInfo;
  host: HostInfo;
  evidenceDir: string;
  filePrefix: string;
  started: string;
  finished: string;
  status: "completed" | "partial" | "cancelled";
  jobs: JobOutcome[];
  footprint: {
    toolPath: string;
    toolSha256: string;
    toolMedia: string;
    outputMedia: string;
    outputOnSystemDisk: boolean;
    processes: ProcInfo[];
    notes: string[];
  };
  logFile: string;
  reportFile: string;
  summaryFile: string;
}

export type CollectEvent =
  | { kind: "prepared"; evidenceDir: string; jobs: JobPlan[] }
  | { kind: "jobStarted"; index: number }
  | { kind: "progress"; index: number; progress: Progress }
  | { kind: "jobFinished"; index: number; outcome: JobOutcome }
  | ({ kind: "log" } & LogEntry);

export type FinishEvent = { kind: "done"; summary: CollectSummary } | { kind: "failed"; message: string };

// -- 네트워크 수신 (ws-core net::RecvEvent) --
export type RecvEvent =
  | { kind: "listening"; addrs: string[]; code: string }
  | { kind: "authFailed"; peer: string; reason: string; count: number; delaySecs: number }
  | { kind: "rejected"; peer: string; reason: string }
  | { kind: "accepted"; peer: string; case: CaseInfo; host: string; evidenceDir: string; needBytes: number }
  | { kind: "fileOpened"; name: string }
  | { kind: "fileClosed"; name: string; size: number }
  | { kind: "fileIncomplete"; name: string; size: number }
  | { kind: "received"; total: number; need: number; rateBps: number }
  | { kind: "verifying"; done: number; total: number }
  | { kind: "verifyDone" }
  | { kind: "finished"; status: string }
  | { kind: "disconnected"; reason: string; waitSecs: number }
  | { kind: "resumed"; peer: string; count: number };

export interface ReceiveOutcome {
  evidenceDir: string;
  status: string;
  files: [string, number][];
  incomplete: [string, number][];
  resumes: number;
}

export type ReceiveFinish = { kind: "done"; outcome: ReceiveOutcome } | { kind: "stopped" } | { kind: "failed"; message: string };
