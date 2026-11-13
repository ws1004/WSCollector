// 표시용 포맷 (ws-core util과 같은 기준: 이진 단위)
export function size(bytes: number | string): string {
  const b = typeof bytes === "string" ? Number(bytes) : bytes;
  const K = 1024;
  if (b >= K ** 4) return (b / K ** 4).toFixed(2) + " TB";
  if (b >= K ** 3) return (b / K ** 3).toFixed(2) + " GB";
  if (b >= K ** 2) return Math.round(b / K ** 2) + " MB";
  if (b >= K) return Math.round(b / K) + " KB";
  return b + " B";
}

export function rate(bps: number): string {
  const K = 1024;
  if (bps >= K ** 3) return (bps / K ** 3).toFixed(2) + " GB/s";
  if (bps >= K ** 2) return (bps / K ** 2).toFixed(1) + " MB/s";
  return Math.round(bps / K) + " KB/s";
}

const pad = (n: number) => String(n).padStart(2, "0");
export function duration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
}

export const ALG_LABEL = { md5: "MD5", sha1: "SHA-1", sha256: "SHA-256" } as const;
