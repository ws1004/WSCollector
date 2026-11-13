import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 개발 서버 설정: 고정 포트, Rust 소스 변경은 감시하지 않음
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**", "**/design/**"] },
  },
  build: {
    target: "es2022",
    // 글꼴을 포함한 모든 자산을 앱에 내장 (대상 PC에서 인터넷 접속 금지)
    assetsInlineLimit: 0,
  },
});
