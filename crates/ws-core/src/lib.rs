//! WS Collector 수집 엔진.
//!
//! GUI(Tauri)와 CLI가 같은 엔진을 쓰므로, 결과물(폴더 구조, 파일 이름, 해시, 로그)이 항상 같습니다.
//! OS별 구현은 `platform` 아래에 둡니다.

pub mod case;
pub mod collect;
pub mod e01;
pub mod hash;
pub mod i18n;
pub mod imaging;
pub mod inventory;
pub mod logbook;
pub mod memory;
pub mod model;
pub mod net;
pub mod platform;
pub mod report;
pub mod source;
pub mod store;
pub mod target;
pub mod util;

/// 엔진 버전. 보고서와 수집 로그에 기록합니다.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
