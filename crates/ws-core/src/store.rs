//! 증거 저장소: 이미지, 로그, 요약, 보고서를 어디에 쓰는지 추상화합니다.
//!
//! - `LocalStore`: 이 컴퓨터의 증거 폴더 (USB, 외장 디스크 등)
//! - `net::RemoteStore`: 네트워크로 연결된 수신 PC의 증거 폴더 (대상 시스템 디스크 쓰기 0)
//!
//! 두 저장소 모두 같은 규칙을 지킵니다: 기존 파일은 절대 덮어쓰지 않고(create_new),
//! 파일을 닫을 때 저장 매체에 실제로 기록(sync)한 뒤 다음으로 넘어갑니다.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};

use crate::hash::{Digests, HashAlg, HashPipeline};
use crate::imaging::{ImageFormat, OutputFile, CHUNK};
use crate::logbook::LogLevel;
use crate::source::{drop_cache, is_alignment_error, open_for_verify, read_aligned, AlignedBuf};

/// 저장소에 만든 파일 하나
pub trait StoreFile: Send {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// 저장 매체에 기록을 마치고 닫습니다
    fn finish(self: Box<Self>) -> io::Result<()>;
}

/// 검증 진행 알림 (읽은 바이트, 전체 바이트)
pub type VerifyProgress<'a> = &'a mut dyn FnMut(u64, u64);
/// 검증 중 알릴 로그
pub type VerifyLog<'a> = &'a mut dyn FnMut(LogLevel, &str);

pub trait EvidenceStore: Send + Sync {
    /// 사람이 읽는 저장 위치 (로그, 보고서용)
    fn location(&self) -> String;
    /// 새 파일을 만듭니다. 같은 이름이 있으면 실패해야 합니다
    fn create(&self, name: &str) -> io::Result<Box<dyn StoreFile>>;
    /// 저장한 파일들을 캐시 없이 다시 읽어 해시를 계산합니다 (E01은 풀어서). 중단되면 `Ok(None)`.
    /// 돌려주는 값: (해시, 원본 기준 바이트 수)
    fn verify(
        &self,
        files: &[OutputFile],
        format: ImageFormat,
        algs: &[HashAlg],
        cancel: &AtomicBool,
        progress: VerifyProgress,
        log: VerifyLog,
    ) -> Result<Option<(Digests, u64)>>;
    /// 수집 세션을 마칩니다 (네트워크: 수신 측에 종료 알림)
    fn finish_session(&self, _status: &str) -> Result<()> {
        Ok(())
    }
    /// 끝나지 않은 파일 표시: 이름 뒤에 `.incomplete`를 붙임. 바뀐 이름을 돌려줌 (지원하지 않으면 None)
    fn mark_incomplete(&self, _name: &str) -> io::Result<Option<String>> {
        Ok(None)
    }
    /// 저장소에서 생긴 일 (네트워크 끊김, 이어받기 등). 수집 로그에 남김
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }
}

/// 저장 파일 이름 검사: 증거 폴더 밖으로 나가는 이름을 막습니다 (수신 측 보호에도 사용)
pub fn check_file_name(name: &str) -> io::Result<()> {
    // Windows 예약 장치 이름 (NUL.dd를 열면 장치가 열려 데이터가 버려짐)
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    let bad = name.is_empty()
        || name.chars().count() > 200
        || name.starts_with('.')
        || name.ends_with('.')
        || name.ends_with(' ')
        || reserved
        || name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() || crate::util::is_bidi(c));
    if bad {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("허용하지 않는 파일 이름: {name}")));
    }
    Ok(())
}

pub struct LocalStore {
    dir: PathBuf,
}

impl LocalStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

struct LocalFile(File);

impl StoreFile for LocalFile {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.write_all(data)
    }
    fn finish(mut self: Box<Self>) -> io::Result<()> {
        self.0.flush()?;
        self.0.sync_all()
    }
}

impl EvidenceStore for LocalStore {
    fn location(&self) -> String {
        self.dir.display().to_string()
    }

    fn create(&self, name: &str) -> io::Result<Box<dyn StoreFile>> {
        check_file_name(name)?;
        let f = OpenOptions::new().write(true).create_new(true).open(self.dir.join(name))?;
        Ok(Box::new(LocalFile(f)))
    }

    fn mark_incomplete(&self, name: &str) -> io::Result<Option<String>> {
        check_file_name(name)?;
        let new = format!("{name}.incomplete");
        let to = self.dir.join(&new);
        if to.exists() {
            return Ok(None);
        }
        std::fs::rename(self.dir.join(name), to)?;
        Ok(Some(new))
    }

    fn verify(
        &self,
        files: &[OutputFile],
        format: ImageFormat,
        algs: &[HashAlg],
        cancel: &AtomicBool,
        progress: VerifyProgress,
        log: VerifyLog,
    ) -> Result<Option<(Digests, u64)>> {
        verify_dir(&self.dir, files, format, algs, cancel, progress, log)
    }
}

/// 폴더 안의 파일들을 순서대로 다시 읽어 해시를 계산합니다 (로컬 저장, 네트워크 수신 측 공용)
pub fn verify_dir(
    dir: &Path,
    files: &[OutputFile],
    format: ImageFormat,
    algs: &[HashAlg],
    cancel: &AtomicBool,
    progress: VerifyProgress,
    log: VerifyLog,
) -> Result<Option<(Digests, u64)>> {
    if files.len() > 100_000 {
        return Err(anyhow!("검증할 파일이 너무 많습니다 ({}개)", files.len()));
    }
    if format == ImageFormat::E01 {
        return crate::e01::verify_e01(dir, files, algs, cancel, progress, log);
    }
    let total = files.iter().try_fold(0u64, |a, f| a.checked_add(f.size)).ok_or_else(|| anyhow!("파일 크기 합이 너무 큽니다"))?;
    let pipe = HashPipeline::new(algs);
    let mut buf = AlignedBuf::new(CHUNK);
    let mut done: u64 = 0;
    let mut warned = false;
    let mut last = Instant::now();

    for of in files {
        check_file_name(&of.name)?;
        let path = dir.join(&of.name);
        // 실제 파일 길이가 기록과 다르면(덧붙여 쓰임, 잘림) 앞부분만 읽고 "일치"가 나오지 않게 먼저 확인
        let real = std::fs::metadata(&path).with_context(|| format!("검증용 파일 없음: {}", of.name))?.len();
        if real != of.size {
            return Err(anyhow!("파일 크기가 기록과 다릅니다: {} (기록 {} 바이트, 실제 {} 바이트)", of.name, of.size, real));
        }
        let mut warn = |log: &mut dyn FnMut(LogLevel, &str), msg: &str| {
            if !warned {
                log(LogLevel::Warn, msg);
                warned = true;
            }
        };
        // 캐시를 거치지 않는 읽기가 안 되는 매체(일부 네트워크, FUSE 경로)는 일반 읽기로 대체하고 기록
        let mut cached = false;
        let mut file = match open_for_verify(&path) {
            Ok(f) => f,
            Err(_) => {
                warn(log, "저장 매체가 캐시 없는 읽기를 지원하지 않아 일반 읽기로 검증합니다 (캐시를 비운 뒤 읽음)");
                cached = true;
                let f = File::open(&path).with_context(|| format!("검증용 열기 실패: {}", of.name))?;
                drop_cache(&f, 0, 0);
                f
            }
        };
        let mut pos: u64 = 0;
        while pos < of.size {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let n = match read_aligned(&file, pos, &mut buf[..]) {
                Ok(n) => n,
                // 파일마다 따로 판단 (앞 파일에서 대체했다고 다음 파일 오류를 그냥 실패로 두지 않음)
                Err(e) if is_alignment_error(&e) && !cached => {
                    warn(log, "캐시 없는 읽기가 거부되어 일반 읽기로 검증을 계속합니다 (캐시를 비운 뒤 읽음)");
                    cached = true;
                    file = File::open(&path)?;
                    drop_cache(&file, 0, 0);
                    continue;
                }
                Err(e) => return Err(anyhow!("검증 중 읽기 실패 ({}): {e}", of.name)),
            };
            if n == 0 {
                return Err(anyhow!("검증 중 파일이 예상보다 짧습니다: {}", of.name));
            }
            let n = (n as u64).min(of.size - pos) as usize;
            pipe.feed(&Arc::from(&buf[..n]));
            pos += n as u64;
            done += n as u64;
            if last.elapsed().as_millis() >= 200 || done == total {
                progress(done, total);
                last = Instant::now();
            }
        }
    }
    Ok(Some((pipe.finish()?, done)))
}
