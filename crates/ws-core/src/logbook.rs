//! 수집 로그 (`collection.log`).
//!
//! 모든 줄은 현지 시각(시간대 포함)과 함께 저장 폴더의 로그 파일에 바로 기록하고,
//! GUI, CLI에도 같은 내용을 전달합니다.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use crate::store::StoreFile;

use serde::Serialize;

use crate::util::{fmt_ts, now_local};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn tag(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub time: String,
    pub level: LogLevel,
    pub message: String,
}

pub struct LogBook {
    file: Mutex<Option<Box<dyn StoreFile>>>,
    entries: Mutex<Vec<LogEntry>>,
}

impl LogBook {
    /// 파일 없이 메모리에만 기록 (저장 폴더를 만들기 전 단계)
    pub fn memory() -> Self {
        Self { file: Mutex::new(None), entries: Mutex::new(Vec::new()) }
    }

    /// 로그 파일을 열고, 그 전에 메모리에 쌓인 줄도 함께 기록합니다.
    pub fn attach_file(&self, path: &Path) -> std::io::Result<()> {
        struct Append(std::fs::File);
        impl StoreFile for Append {
            fn write_all(&mut self, d: &[u8]) -> std::io::Result<()> {
                self.0.write_all(d)?;
                self.0.flush()
            }
            fn finish(self: Box<Self>) -> std::io::Result<()> {
                self.0.sync_all()
            }
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        self.attach(Box::new(Append(f)))
    }

    /// 저장소(로컬, 네트워크)의 파일에 로그를 이어서 기록합니다. 그 전에 메모리에 쌓인 줄도 함께 씁니다.
    pub fn attach(&self, mut f: Box<dyn StoreFile>) -> std::io::Result<()> {
        let mut head = String::new();
        for e in self.entries.lock().unwrap().iter() {
            head.push_str(&format!("{} [{}] {}\n", e.time, e.level.tag(), e.message));
        }
        if !head.is_empty() {
            f.write_all(head.as_bytes())?;
        }
        *self.file.lock().unwrap() = Some(f);
        Ok(())
    }

    /// 로그 파일을 닫습니다 (저장 매체에 기록 완료)
    pub fn close(&self) -> std::io::Result<()> {
        // 잠금을 먼저 풀고 닫음 (네트워크 저장이면 닫기가 왕복이라 잠금을 오래 쥐지 않게)
        let f = self.file.lock().unwrap().take();
        match f {
            Some(f) => f.finish(),
            None => Ok(()),
        }
    }

    pub fn write(&self, level: LogLevel, message: &str) -> LogEntry {
        // 한 항목은 한 줄: 메시지 안의 줄바꿈, 서식 문자로 가짜 로그 줄이 생기지 않게
        let entry = LogEntry { time: fmt_ts(&now_local()), level, message: crate::util::clean_text(message) };
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            // 로그 기록 실패가 수집 자체를 멈추게 하지는 않음
            let _ = f.write_all(format!("{} [{}] {}\n", entry.time, level.tag(), entry.message).as_bytes());
        }
        self.entries.lock().unwrap().push(entry.clone());
        entry
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.lock().unwrap().clone()
    }
}
