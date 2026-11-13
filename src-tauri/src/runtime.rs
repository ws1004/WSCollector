//! 실행 위치 기준 runtime 폴더 격리.
//!
//! ```text
//! <USB>/WSCollector/
//!   WSCollector(.exe)
//!   runtime/
//!     webview-data/   Windows: WebView2 사용자 데이터 (원래 %LOCALAPPDATA%)
//!                     Linux: WebKitGTK 데이터 (원래 ~/.local/share)
//!     cache/          Linux: WebKitGTK 캐시 (원래 ~/.cache)
//!     config/         Linux: GTK 설정 (원래 ~/.config)
//!     tmp/            TEMP/TMP (Windows), TMPDIR (Linux)
//!     webview2/       (선택, Windows) WebView2 Fixed Version 런타임
//! ```

use std::path::PathBuf;

pub struct Runtime {
    pub base: PathBuf,
    pub webview_data: PathBuf,
}

/// 실행 파일이 있는 폴더
pub fn base_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf())
}

/// 환경 변수를 설정합니다. 웹뷰 생성 전, 다른 스레드가 생기기 전에 호출해야 합니다.
pub fn isolate() -> anyhow::Result<Runtime> {
    let exe_dir = base_dir().ok_or_else(|| anyhow::anyhow!("실행 파일 위치를 알 수 없습니다"))?;
    let base = exe_dir.join("runtime");
    let webview_data = base.join("webview-data");
    let tmp = base.join("tmp");
    std::fs::create_dir_all(&webview_data)?;
    std::fs::create_dir_all(&tmp)?;

    // 프로세스 시작 직후 단일 스레드 상태에서만 호출합니다
    #[cfg(windows)]
    {
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &webview_data);
        std::env::set_var("TEMP", &tmp);
        std::env::set_var("TMP", &tmp);
        let fixed = base.join("webview2");
        if fixed.join("msedgewebview2.exe").is_file() {
            std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", &fixed);
        }
    }
    #[cfg(not(windows))]
    {
        // WebKitGTK, GTK가 홈 폴더(~/.cache, ~/.local/share, ~/.config)에 쓰는 것을 실행 위치로 돌림
        let cache = base.join("cache");
        let config = base.join("config");
        std::fs::create_dir_all(&cache)?;
        std::fs::create_dir_all(&config)?;
        std::env::set_var("XDG_DATA_HOME", &webview_data);
        std::env::set_var("XDG_CACHE_HOME", &cache);
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("TMPDIR", &tmp);
        // XDG를 따르지 않고 홈 폴더에 직접 쓰는 것(그래픽 드라이버 셰이더 캐시 등)까지 막기 위해
        // HOME과 셰이더 캐시 경로도 실행 위치로 돌림
        let home = base.join("home");
        std::fs::create_dir_all(&home)?;
        std::env::set_var("HOME", &home);
        std::env::set_var("MESA_SHADER_CACHE_DIR", &cache);
        std::env::set_var("__GL_SHADER_DISK_CACHE_PATH", &cache);
        // GPU 합성 기능이 없는 환경(가상 머신, 원격 화면)에서 빈 창이 뜨는 문제 방지
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }
    }

    Ok(Runtime { base, webview_data })
}
