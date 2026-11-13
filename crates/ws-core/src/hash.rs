//! 해시 계산. 알고리즘마다 스레드 하나씩 돌려서 디스크 읽기 속도를 따라가게 합니다.

use std::collections::BTreeMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlg {
    Md5,
    Sha1,
    Sha256,
}

impl HashAlg {
    pub const ALL: [HashAlg; 3] = [HashAlg::Md5, HashAlg::Sha1, HashAlg::Sha256];

    pub fn label(self) -> &'static str {
        match self {
            HashAlg::Md5 => "MD5",
            HashAlg::Sha1 => "SHA-1",
            HashAlg::Sha256 => "SHA-256",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('-', "").as_str() {
            "md5" => Some(HashAlg::Md5),
            "sha1" => Some(HashAlg::Sha1),
            "sha256" => Some(HashAlg::Sha256),
            _ => None,
        }
    }
}

enum Hasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
}

impl Hasher {
    fn new(alg: HashAlg) -> Self {
        match alg {
            HashAlg::Md5 => Hasher::Md5(Md5::new()),
            HashAlg::Sha1 => Hasher::Sha1(Sha1::new()),
            HashAlg::Sha256 => Hasher::Sha256(Sha256::new()),
        }
    }
    fn update(&mut self, d: &[u8]) {
        match self {
            Hasher::Md5(h) => h.update(d),
            Hasher::Sha1(h) => h.update(d),
            Hasher::Sha256(h) => h.update(d),
        }
    }
    fn hex(self) -> String {
        fn hx(b: &[u8]) -> String {
            b.iter().map(|x| format!("{x:02x}")).collect()
        }
        match self {
            Hasher::Md5(h) => hx(&h.finalize()),
            Hasher::Sha1(h) => hx(&h.finalize()),
            Hasher::Sha256(h) => hx(&h.finalize()),
        }
    }
}

/// 알고리즘 → 16진수 해시
pub type Digests = BTreeMap<HashAlg, String>;

/// 데이터 조각을 받아 여러 알고리즘을 병렬로 계산하는 파이프라인
pub struct HashPipeline {
    senders: Vec<SyncSender<Arc<[u8]>>>,
    workers: Vec<(HashAlg, JoinHandle<String>)>,
}

/// 스레드마다 대기열 깊이. 4MB 청크 기준 알고리즘당 최대 12MB.
const DEPTH: usize = 3;

impl HashPipeline {
    pub fn new(algs: &[HashAlg]) -> Self {
        let mut senders = Vec::new();
        let mut workers = Vec::new();
        for &alg in algs {
            let (tx, rx): (SyncSender<Arc<[u8]>>, Receiver<Arc<[u8]>>) = sync_channel(DEPTH);
            let h = std::thread::Builder::new()
                .name(format!("hash-{}", alg.label()))
                .spawn(move || {
                    let mut hasher = Hasher::new(alg);
                    for chunk in rx {
                        hasher.update(&chunk);
                    }
                    hasher.hex()
                })
                .expect(crate::tr!("해시 스레드 생성 실패", "failed to spawn hash thread"));
            senders.push(tx);
            workers.push((alg, h));
        }
        Self { senders, workers }
    }

    pub fn feed(&self, chunk: &Arc<[u8]>) {
        for tx in &self.senders {
            // 받는 쪽이 끝났다면(패닉) finish에서 드러나므로 여기서는 무시
            let _ = tx.send(chunk.clone());
        }
    }

    /// 해시 스레드가 비정상 종료하면 오류 (잘못된 해시가 "일치"로 보이지 않게)
    pub fn finish(self) -> anyhow::Result<Digests> {
        drop(self.senders);
        let mut out = Digests::new();
        for (alg, h) in self.workers {
            let v = h.join().map_err(|_| {
                anyhow::anyhow!(crate::tr!(
                    format!("{} 해시 계산 스레드가 비정상 종료했습니다", alg.label()),
                    format!("{} hash thread terminated abnormally", alg.label())
                ))
            })?;
            out.insert(alg, v);
        }
        Ok(out)
    }
}

/// 한 번에 계산 (작은 데이터용: 도구 실행 파일 해시 등)
pub fn digest_file(path: &std::path::Path, alg: HashAlg) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Hasher::new(alg);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let p = HashPipeline::new(&HashAlg::ALL);
        let a: Arc<[u8]> = Arc::from(&b"ab"[..]);
        let b: Arc<[u8]> = Arc::from(&b"c"[..]);
        p.feed(&a);
        p.feed(&b);
        let d = p.finish().unwrap();
        assert_eq!(d[&HashAlg::Md5], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(d[&HashAlg::Sha1], "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(d[&HashAlg::Sha256], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
