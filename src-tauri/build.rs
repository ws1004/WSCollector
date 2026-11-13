//! 빌드 스크립트 (WSImager에서 이식).
//!
//! 관리자 권한 매니페스트(app.manifest)를 파일 참조 방식으로 넣습니다.
//! tauri-build의 set_manifest()는 RC 컴파일러가 UTF-16 변환을 하면서 잘못된 바이트를 넣어
//! SxS 오류 14001이 나므로 쓰지 않습니다.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=app.manifest");

    let attrs = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attrs).expect("tauri-build 실패");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
        let app_manifest = manifest_dir.join("app.manifest");
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let rc_path = out_dir.join("manifest_only.rc");
        let rc_content = format!(
            "1 24 \"{}\"\n",
            app_manifest.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(&rc_path, rc_content).unwrap();
        embed_resource::compile(&rc_path, embed_resource::NONE)
            .manifest_required()
            .expect("RC.EXE를 찾을 수 없습니다. Windows SDK가 설치되어 있는지 확인하세요");
    }
}
