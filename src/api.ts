// Tauri 커맨드, 이벤트 연결
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { CollectEvent, CollectRequest, DiskInfo, Environment, FinishEvent, OutputCheck, ReceiveFinish, RecvEvent } from "./types";

export const api = {
  environment: () => invoke<Environment>("get_environment"),
  listDisks: (outDir: string | null) => invoke<DiskInfo[]>("list_disks", { outDir }),
  checkOutput: (path: string) => invoke<OutputCheck>("check_output", { path }),
  start: (request: CollectRequest) => invoke<void>("start_collection", { request }),
  cancel: () => invoke<boolean>("cancel_collection"),
  reveal: (path: string) => invoke<void>("reveal_path", { path }),
  pickFolder: async (): Promise<string | null> => {
    const r = await open({ directory: true, multiple: false, title: "증거를 저장할 폴더 선택" });
    return typeof r === "string" ? r : null;
  },
  onCollect: (fn: (e: CollectEvent) => void): Promise<UnlistenFn> => listen<CollectEvent>("collect", (e) => fn(e.payload)),
  onFinish: (fn: (e: FinishEvent) => void): Promise<UnlistenFn> => listen<FinishEvent>("collect-finish", (e) => fn(e.payload)),
  startReceive: (outBase: string, port: number, bind: string | null) => invoke<void>("start_receive", { outBase, port, bind }),
  stopReceive: () => invoke<boolean>("stop_receive"),
  onCloseBlocked: (fn: (what: string) => void): Promise<UnlistenFn> => listen<string>("close-blocked", (e) => fn(e.payload)),
  onReceive: (fn: (e: RecvEvent) => void): Promise<UnlistenFn> => listen<RecvEvent>("receive", (e) => fn(e.payload)),
  onReceiveFinish: (fn: (e: ReceiveFinish) => void): Promise<UnlistenFn> => listen<ReceiveFinish>("receive-finish", (e) => fn(e.payload)),
};
