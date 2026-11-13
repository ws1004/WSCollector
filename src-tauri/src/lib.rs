//! WS Collector GUI.
//!
//! 흔적 최소화 원칙(CLAUDE.md 주의사항 2, 3):
//! - WebView2 데이터 폴더, 임시 폴더를 실행 파일 옆 `runtime/`(= 수집 USB)로 돌립니다.
//! - USB에 WebView2 휴대용 런타임(`runtime/webview2/`)이 있으면 그것을 씁니다.
//! - 웹뷰는 incognito, 디스크 캐시 최소화로 띄웁니다.
//! - AppData에 쓰는 플러그인(store, window-state, log)은 쓰지 않습니다.

mod commands;
mod runtime;

use tauri::{WebviewUrl, WebviewWindowBuilder};

const BROWSER_ARGS: &str = concat!(
    "--disable-features=msWebOOUI, msPdfOOUI, msSmartScreenProtection ",
    "--disk-cache-size=1 --media-cache-size=1 ",
    "--disable-gpu-shader-disk-cache --disable-breakpad ",
    "--no-first-run --no-default-browser-check"
);

pub fn run() {
    // 반드시 웹뷰를 만들기 전에 실행해야 합니다 (자식 프로세스 msedgewebview2.exe가 환경 변수를 물려받음)
    let rt = runtime::isolate();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match &rt {
        Ok(r) => log::info!("runtime 격리: {}", r.base.display()),
        Err(e) => log::warn!("runtime 격리 실패, 기본 경로 사용: {e}"),
    }
    let data_dir = rt.as_ref().ok().map(|r| r.webview_data.clone());

    #[allow(unused_mut)]
    let mut args = BROWSER_ARGS.to_string();
    // 개발 빌드 전용: 화면 자동 점검용 원격 디버깅 포트 (릴리스에는 포함되지 않음)
    #[cfg(debug_assertions)]
    if let Ok(port) = std::env::var("WS_DEBUG_PORT") {
        if port.parse::<u16>().is_ok() {
            args.push_str(&format!(" --remote-debugging-port={port}"));
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::Session::default())
        // 수집, 수신 중에 창을 닫으면 증거 파일이 끝나지 않은 채 남으므로 막고 화면에 알림
        // (받다 만 파일은 중지 처리를 거쳐야 .incomplete로 표시됨)
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                use tauri::Manager;
                if let Some(what) = commands::busy(window.app_handle()) {
                    api.prevent_close();
                    use tauri::Emitter;
                    let _ = window.emit("close-blocked", what);
                }
            }
        })
        .setup(move |app| {
            #[allow(unused_mut)]
            let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("WS Collector")
                .inner_size(1440.0, 900.0)
                .min_inner_size(1100.0, 700.0)
                .incognito(true);
            // 브라우저 인자는 WebView2(Windows) 전용
            #[cfg(windows)]
            {
                builder = builder.additional_browser_args(&args);
            }
            #[cfg(not(windows))]
            let _ = &args;
            if let Some(dir) = &data_dir {
                builder = builder.data_directory(dir.clone());
            }
            builder.build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_environment,
            commands::list_disks,
            commands::check_output,
            commands::start_collection,
            commands::cancel_collection,
            commands::reveal_path,
            commands::start_receive,
            commands::stop_receive,
        ])
        .run(tauri::generate_context!())
        .expect("WS Collector 실행 중 오류");
}
