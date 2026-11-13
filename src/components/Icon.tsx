// 아이콘 스프라이트 (design/fluent와 같은 Lucide 계열 선 아이콘)
export type IconName =
  | "disk" | "usb" | "mem" | "pc" | "check" | "check-c" | "shield" | "refresh" | "alert" | "info"
  | "play" | "stop" | "folder" | "lock" | "copy" | "file" | "clock" | "x" | "net" | "download" | "send";

export function Icon({ name, className = "ic" }: { name: IconName; className?: string }) {
  return (
    <svg className={className} aria-hidden="true">
      <use href={`#i-${name}`} />
    </svg>
  );
}

const S = { fill: "none", stroke: "currentColor", strokeWidth: 1.75, strokeLinecap: "round", strokeLinejoin: "round" } as const;

export function IconSprite() {
  return (
    <svg width="0" height="0" style={{ position: "absolute" }} aria-hidden="true">
      <defs>
        <symbol id="i-disk" viewBox="0 0 24 24" {...S}><path d="M22 12H2" /><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" /><path d="M6 16h.01M10 16h.01" /></symbol>
        <symbol id="i-usb" viewBox="0 0 24 24" {...S}><rect x="7" y="9" width="10" height="13" rx="2" /><path d="M9 9V3h6v6M11 5.5h.01M13 5.5h.01" /></symbol>
        <symbol id="i-mem" viewBox="0 0 24 24" {...S}><rect x="3" y="6" width="18" height="12" rx="2" /><path d="M7 10v4M11 10v4M15 10v4M7 18v3M17 18v3M7 3v3M17 3v3" /></symbol>
        <symbol id="i-pc" viewBox="0 0 24 24" {...S}><rect x="2" y="3" width="20" height="14" rx="2" /><path d="M8 21h8M12 17v4" /></symbol>
        <symbol id="i-check" viewBox="0 0 24 24" {...S} strokeWidth={2.5}><path d="M20 6 9 17l-5-5" /></symbol>
        <symbol id="i-check-c" viewBox="0 0 24 24" {...S}><circle cx="12" cy="12" r="10" /><path d="m8 12 3 3 5-6" /></symbol>
        <symbol id="i-shield" viewBox="0 0 24 24" {...S}><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /><path d="m9 12 2 2 4-4" /></symbol>
        <symbol id="i-refresh" viewBox="0 0 24 24" {...S}><path d="M21 12a9 9 0 1 1-2.64-6.36L21 8" /><path d="M21 3v5h-5" /></symbol>
        <symbol id="i-alert" viewBox="0 0 24 24" {...S}><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3z" /><path d="M12 9v4M12 17h.01" /></symbol>
        <symbol id="i-info" viewBox="0 0 24 24" {...S}><circle cx="12" cy="12" r="10" /><path d="M12 16v-4M12 8h.01" /></symbol>
        <symbol id="i-play" viewBox="0 0 24 24" fill="currentColor"><path d="M7 4.5v15a1 1 0 0 0 1.5.86l12-7.5a1 1 0 0 0 0-1.72l-12-7.5A1 1 0 0 0 7 4.5z" /></symbol>
        <symbol id="i-stop" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="6" width="12" height="12" rx="2" /></symbol>
        <symbol id="i-folder" viewBox="0 0 24 24" {...S}><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2z" /></symbol>
        <symbol id="i-lock" viewBox="0 0 24 24" {...S}><rect x="4" y="11" width="16" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></symbol>
        <symbol id="i-copy" viewBox="0 0 24 24" {...S}><rect x="9" y="9" width="12" height="12" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></symbol>
        <symbol id="i-file" viewBox="0 0 24 24" {...S}><path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z" /><path d="M14 2v6h6M8 13h8M8 17h5" /></symbol>
        <symbol id="i-clock" viewBox="0 0 24 24" {...S}><circle cx="12" cy="12" r="10" /><path d="M12 6v6l4 2" /></symbol>
        <symbol id="i-x" viewBox="0 0 24 24" {...S}><path d="M18 6 6 18M6 6l12 12" /></symbol>
        <symbol id="i-net" viewBox="0 0 24 24" {...S}><rect x="16" y="16" width="6" height="6" rx="1" /><rect x="2" y="16" width="6" height="6" rx="1" /><rect x="9" y="2" width="6" height="6" rx="1" /><path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3M12 12V8" /></symbol>
        <symbol id="i-download" viewBox="0 0 24 24" {...S}><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" /><path d="m7 10 5 5 5-5M12 15V3" /></symbol>
        <symbol id="i-send" viewBox="0 0 24 24" {...S}><path d="m22 2-7 20-4-9-9-4Z" /><path d="M22 2 11 13" /></symbol>
      </defs>
    </svg>
  );
}
