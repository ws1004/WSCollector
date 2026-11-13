// 릴리스 빌드에서는 콘솔 창을 띄우지 않습니다
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    ws_gui_lib::run();
}
