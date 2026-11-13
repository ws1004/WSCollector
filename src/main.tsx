import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// 글꼴은 npm 패키지로 앱에 내장합니다 (CDN 사용 금지: 대상 PC에서 외부 접속 흔적 방지)
import "pretendard/dist/web/variable/pretendardvariable-dynamic-subset.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "./styles/theme.css";
import "./styles/screens.css";
import "./styles/app.css";
import App from "./App";
import { startDevBridge } from "./devbridge";

// 시험 빌드에서만 동작 (VITE_WS_BRIDGE 지정 시). 일반 빌드에서는 아무것도 하지 않음
startDevBridge();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
