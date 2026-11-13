//! 네트워크 전송: 대상 시스템에서 읽은 이미지를 분석 PC(수신 측)에 바로 저장합니다.
//!
//! ```text
//! [수신 PC]  wscollector-cli receive --out D:\Cases          → 연결 코드 표시, 접속 대기
//! [대상]     wscollector-cli image -s sda --send IP:7700 ...  → 연결 코드 입력
//! ```
//!
//! - 대상 시스템이 수신 PC로 접속합니다 (서버 방화벽은 들어오는 연결을 막는 경우가 많음).
//! - 연결 코드로 SPAKE2 키 합의를 해서, 코드를 모르는 쪽은 연결할 수 없고 엿들어도 코드를 알아낼 수 없습니다.
//!   이후 모든 메시지는 ChaCha20-Poly1305로 암호화, 무결성 확인합니다 (방향별 키, 순번 nonce, 키는 핸드셰이크 내용에 묶음).
//! - 이미지, 로그, 요약, 보고서는 모두 수신 PC의 `저장 폴더/사건 번호/증거 번호`에 저장됩니다.
//!   대상 시스템 디스크에는 아무것도 쓰지 않습니다.
//! - 검증은 수신 PC가 저장한 파일을 캐시 없이 다시 읽어 해시를 계산하고, 대상 쪽이 원본 해시와 비교합니다.
//!   파일을 닫을 때 수신 측이 실제로 쓴 바이트 수도 보낸 바이트 수와 대조합니다.
//! - 연결이 조용히 끊겨도 멈추지 않도록: 보내는 쪽은 쉬는 동안 15초마다 신호를 보내고,
//!   양쪽 모두 2분 동안 아무것도 오가지 않으면 오류로 끝냅니다.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::case::CaseInfo;
use crate::hash::{Digests, HashAlg};
use crate::imaging::{ImageFormat, OutputFile};
use crate::logbook::LogLevel;
use crate::store::{check_file_name, verify_dir, EvidenceStore, StoreFile, VerifyLog, VerifyProgress};
use crate::util::format_bytes;

pub const DEFAULT_PORT: u16 = 7700;
/// 프로토콜 버전 (MAGIC에도 들어 있어 버전이 다르면 핸드셰이크에서 바로 알 수 있음)
/// 2: E01 (검증 요청에 형식 추가)
pub const PROTOCOL: u32 = 2;
const MAGIC: &[u8; 8] = b"WSCNET01";
/// 데이터 프레임 하나의 최대 본문 크기
const DATA_FRAME: usize = 1 << 20;
/// 암호문 프레임 최대 크기 (본문 + 머리 + 태그 여유)
const MAX_FRAME: usize = DATA_FRAME + 4096;
/// 인증 전 단계(핸드셰이크 + 첫 메시지) 전체 마감 시간: 느리게 보내며 수신기를 붙잡는 접속 차단
const PREAUTH_DEADLINE: Duration = Duration::from_secs(15);
/// 이 시간 동안 아무것도 오가지 않으면 상대가 사라진 것으로 봄
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// 보내는 쪽이 쉬는 동안(불량 섹터 재시도 등) 보내는 신호 간격
const HEARTBEAT: Duration = Duration::from_secs(15);
/// 동시에 열 수 있는 파일 수 (이미지 조각 1개 + 로그 + 요약, 보고서)
const MAX_OPEN_FILES: usize = 8;
/// 받을 크기(need) 외에 허용하는 여유 (로그, 요약, 보고서)
const RECEIVE_SLACK: u64 = 64 << 20;
/// 수신 중 이만큼 쓸 때마다 저장 매체에 기록 (닫을 때 기다리는 시간을 줄이고, 끊겨도 받은 만큼은 남게)
const SYNC_EVERY: u64 = 64 << 20;

// 메시지 종류
const K_HELLO: u8 = 1;
const K_ACCEPT: u8 = 2;
const K_ERROR: u8 = 3;
const K_OK: u8 = 4;
const K_PING: u8 = 5;
/// 끊긴 세션 이어받기 요청, 응답
const K_RESUME: u8 = 6;
const K_RESUMED: u8 = 7;
const K_OPEN: u8 = 10;
const K_DATA: u8 = 11;
const K_CLOSE: u8 = 12;
const K_CLOSED: u8 = 13;
const K_VERIFY: u8 = 20;
const K_VPROGRESS: u8 = 21;
const K_VDONE: u8 = 22;
const K_BYE: u8 = 30;

// -- 연결 코드 -------------------------------------------------------------

/// 헷갈리는 글자(0, 1, I, O)를 뺀 32자
const CODE_CHARS: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const CODE_LEN: usize = 10;

/// 새 일회용 연결 코드 (50비트). 표시 형식 `XXXXX-XXXXX`
pub fn new_code() -> Result<String> {
    let mut b = [0u8; CODE_LEN];
    getrandom::getrandom(&mut b).map_err(|e| anyhow!("{}", crate::tr!(format!("난수를 만들 수 없습니다: {e}"), format!("Cannot generate random numbers: {e}"))))?;
    let s: String = b.iter().map(|x| CODE_CHARS[(x & 31) as usize] as char).collect();
    Ok(format!("{}-{}", &s[..5], &s[5..]))
}

/// 입력한 코드를 정규화 (대소문자, 하이픈, 공백 무시). 형식이 틀리면 오류
pub fn normalize_code(s: &str) -> Result<String> {
    let c: String = s.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect();
    if c.len() != CODE_LEN || !c.bytes().all(|b| CODE_CHARS.contains(&b)) {
        return Err(anyhow!(crate::tr!("연결 코드 형식이 맞지 않습니다 (예: 4KQ7M-2XD9P)", "Invalid connection code format (e.g. 4KQ7M-2XD9P)")));
    }
    Ok(c)
}

// -- 암호화 채널 -----------------------------------------------------------

struct Channel {
    stream: TcpStream,
    tx: ChaCha20Poly1305,
    rx: ChaCha20Poly1305,
    tx_n: u64,
    rx_n: u64,
    /// 인증 전 단계의 마감 시각 (지나면 읽기 실패)
    deadline: Option<Instant>,
    /// 중지 요청 (수신 측). 막혀 있는 읽기를 다른 스레드에서 확실히 깨울 방법이 없어(Windows) 1초마다 확인
    stop: Option<Arc<AtomicBool>>,
}

/// 인증 뒤 읽기를 나누는 단위: 이 간격마다 중지 요청, 유휴 시간을 확인
const READ_SLICE: Duration = Duration::from_secs(1);

fn nonce(n: u64) -> Nonce {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_be_bytes());
    *Nonce::from_slice(&b)
}

/// 마감 시각까지 정확히 읽기. 마감이 없으면 소켓 시간 제한(IDLE_TIMEOUT)을 따름
fn read_exact_by(stream: &mut TcpStream, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<()> {
    let Some(dl) = deadline else { return stream.read_exact(buf) };
    let mut got = 0;
    while got < buf.len() {
        let left = dl.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, crate::tr!("연결 설정 시간 초과", "Connection setup timed out")));
        }
        stream.set_read_timeout(Some(left))?;
        match stream.read(&mut buf[got..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, crate::tr!("상대가 연결을 끊었습니다", "The peer closed the connection"))),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl Channel {
    /// 연결 코드로 키를 합의합니다. `sender`: 이미지를 보내는 쪽(대상 시스템)
    fn handshake(mut stream: TcpStream, code: &str, sender: bool, deadline: Option<Instant>) -> io::Result<Channel> {
        let (state, msg) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.as_bytes()), &Identity::new(b"wscollector-net"));
        let mut hello = Vec::with_capacity(9 + msg.len());
        hello.extend_from_slice(MAGIC);
        hello.push(msg.len() as u8);
        hello.extend_from_slice(&msg);
        stream.write_all(&hello)?;
        let mut head = [0u8; 9];
        read_exact_by(&mut stream, &mut head, deadline)?;
        if &head[..8] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, crate::tr!("WS Collector가 아니거나 프로토콜 버전이 다릅니다", "Not a WS Collector peer, or the protocol version differs")));
        }
        let mut peer = vec![0u8; head[8] as usize];
        read_exact_by(&mut stream, &mut peer, deadline)?;
        let key = state.finish(&peer).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, crate::tr!("키 합의 실패", "Key agreement failed")))?;
        // 키를 이 핸드셰이크 내용(버전, 양쪽 메시지)에 묶음
        let (s_msg, r_msg) = if sender { (&msg, &peer) } else { (&peer, &msg) };
        let mut salt = Vec::with_capacity(8 + s_msg.len() + r_msg.len());
        salt.extend_from_slice(MAGIC);
        salt.extend_from_slice(s_msg);
        salt.extend_from_slice(r_msg);
        let hk = Hkdf::<Sha256>::new(Some(&salt), &key);
        let (mut s2r, mut r2s) = ([0u8; 32], [0u8; 32]);
        hk.expand(b"wscollector sender->receiver", &mut s2r).map_err(|_| io::Error::other(crate::tr!("키 유도 실패", "Key derivation failed")))?;
        hk.expand(b"wscollector receiver->sender", &mut r2s).map_err(|_| io::Error::other(crate::tr!("키 유도 실패", "Key derivation failed")))?;
        let (tx, rx) = if sender { (s2r, r2s) } else { (r2s, s2r) };
        Ok(Channel {
            stream,
            tx: ChaCha20Poly1305::new(Key::from_slice(&tx)),
            rx: ChaCha20Poly1305::new(Key::from_slice(&rx)),
            tx_n: 0,
            rx_n: 0,
            deadline,
            stop: None,
        })
    }

    /// 정확히 buf만큼 읽기.
    /// - 마감이 있으면(인증 전, 오류 이유 읽기) 그때까지만 기다림
    /// - 없으면 1초씩 나눠 읽으며 중지 요청을 확인하고, IDLE_TIMEOUT 동안 한 바이트도 안 오면 시간 초과
    fn read_full(&mut self, buf: &mut [u8]) -> io::Result<()> {
        if self.deadline.is_some() {
            return read_exact_by(&mut self.stream, buf, self.deadline);
        }
        // 데이터가 끊임없이 들어오는 동안에는 읽기가 멈추지 않으므로 매번 확인 (안 하면 받는 중 중지가 안 먹음)
        if self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
            return Err(io::Error::other(stopped_msg()));
        }
        let mut got = 0;
        let mut last = Instant::now();
        while got < buf.len() {
            match self.stream.read(&mut buf[got..]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, crate::tr!("상대가 연결을 끊었습니다", "The peer closed the connection"))),
                Ok(n) => {
                    got += n;
                    last = Instant::now();
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) => {
                    if self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
                        return Err(io::Error::other(stopped_msg()));
                    }
                    if last.elapsed() >= IDLE_TIMEOUT {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, crate::tr!("응답 없음", "No response")));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn send(&mut self, kind: u8, body: &[u8]) -> io::Result<()> {
        let mut plain = Vec::with_capacity(1 + body.len());
        plain.push(kind);
        plain.extend_from_slice(body);
        let ct = self
            .tx
            .encrypt(&nonce(self.tx_n), Payload { msg: &plain, aad: &[] })
            .map_err(|_| io::Error::other(crate::tr!("암호화 실패", "Encryption failed")))?;
        self.tx_n += 1;
        let mut frame = Vec::with_capacity(4 + ct.len());
        frame.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        frame.extend_from_slice(&ct);
        self.stream.write_all(&frame)
    }

    fn send_json<T: Serialize>(&mut self, kind: u8, v: &T) -> io::Result<()> {
        self.send(kind, &serde_json::to_vec(v).map_err(io::Error::other)?)
    }

    /// 프레임 하나 받기. 복호화 실패(코드 불일치, 변조)는 `PermissionDenied`로 구분
    fn recv(&mut self) -> io::Result<(u8, Vec<u8>)> {
        let mut len = [0u8; 4];
        self.read_full(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        if !(17..=MAX_FRAME + 32).contains(&len) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, crate::tr!(format!("잘못된 프레임 크기 {len}"), format!("Invalid frame size {len}"))));
        }
        let mut ct = vec![0u8; len];
        self.read_full(&mut ct)?;
        let plain = self
            .rx
            .decrypt(&nonce(self.rx_n), Payload { msg: &ct, aad: &[] })
            .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, crate::tr!("복호화 실패 (연결 코드가 다르거나 데이터가 변조됨)", "Decryption failed (connection code mismatch or data tampered)")))?;
        self.rx_n += 1;
        let kind = *plain.first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, crate::tr!("빈 메시지", "Empty message")))?;
        Ok((kind, plain[1..].to_vec()))
    }

    /// 인증이 끝나면 마감 대신 유휴 시간 제한을 씀
    fn authenticated(&mut self) {
        self.deadline = None;
        let _ = self.stream.set_read_timeout(Some(READ_SLICE));
        let _ = self.stream.set_write_timeout(Some(IDLE_TIMEOUT));
    }
}

fn json<T: for<'de> Deserialize<'de>>(b: &[u8]) -> io::Result<T> {
    serde_json::from_slice(b).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, crate::tr!(format!("메시지 형식 오류: {e}"), format!("Invalid message format: {e}"))))
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

// -- 메시지 ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub protocol: u32,
    pub tool_version: String,
    pub case: CaseInfo,
    /// 대상 시스템 호스트 이름
    pub host: String,
    pub interface: String,
    #[serde(with = "crate::model::u64_str")]
    pub need_bytes: u64,
    #[serde(with = "crate::model::u64_str")]
    pub split_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    /// 수신 PC의 증거 폴더 전체 경로
    pub evidence_dir: String,
    /// 수신 PC 저장 매체 설명
    pub media: String,
    pub fs: String,
    #[serde(with = "crate::model::u64_str")]
    pub free: u64,
    pub receiver_host: String,
    /// 수신 PC가 본 대상 시스템 주소
    pub peer: String,
    #[serde(default)]
    pub warning: Option<String>,
    /// 이어받기 때 같은 세션인지 확인하는 번호 (수신 측이 정함)
    #[serde(default)]
    pub session: String,
}

#[derive(Serialize, Deserialize)]
struct ResumeMsg {
    session: String,
}
#[derive(Serialize, Deserialize)]
struct FileState {
    id: u32,
    size: u64,
    open: bool,
}
#[derive(Serialize, Deserialize)]
struct ResumedMsg {
    files: Vec<FileState>,
}

#[derive(Serialize, Deserialize)]
struct OpenMsg {
    id: u32,
    name: String,
}
#[derive(Serialize, Deserialize)]
struct IdMsg {
    id: u32,
}
#[derive(Serialize, Deserialize)]
struct ClosedMsg {
    size: u64,
}
#[derive(Serialize, Deserialize)]
struct VerifyMsg {
    files: Vec<OutputFile>,
    #[serde(default)]
    format: ImageFormat,
    algs: Vec<HashAlg>,
}
#[derive(Serialize, Deserialize)]
struct VProgress {
    done: u64,
    total: u64,
}
#[derive(Serialize, Deserialize)]
struct VDone {
    hashes: Digests,
    done: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bye {
    pub status: String,
}

// -- 보내는 쪽 (대상 시스템) -----------------------------------------------

/// 끊긴 뒤 다시 보낼 수 있게 파일마다 보관하는 최근 데이터 (네트워크 버퍼에 걸려 있던 양보다 넉넉히)
const REPLAY_BUF: usize = 32 << 20;
/// 보내는 쪽이 다시 연결을 시도하는 시간
const RESUME_TRY: Duration = Duration::from_secs(10 * 60);
/// 다시 연결 시도 간격
const RESUME_INTERVAL: Duration = Duration::from_secs(3);
/// 마무리 기록(로그, 요약, 보고서) 단계의 재접속 한도: 중단 요청을 받지 않는 대신 짧게
const RESUME_TRY_FINISHING: Duration = Duration::from_secs(60);
/// 접속 거부가 이만큼 이어지면 이어받기 포기 (약 10초)
const REFUSED_GIVE_UP: u32 = 4;
/// 검증 중 끊겨 처음부터 다시 검증하는 최대 횟수
const MAX_VERIFY_RESTARTS: u32 = 3;

/// 수신 측이 알려 온 오류 (요청 하나가 거부됨. 다시 연결해도 해결되지 않음)
#[derive(Debug)]
struct RemoteErr(String);
/// 이어받기로 해결할 수 없는 오류 (프로토콜 위반, 보관 범위 초과, 세션 없음)
#[derive(Debug)]
struct FatalErr(String);

impl std::fmt::Display for RemoteErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RemoteErr {}
impl std::fmt::Display for FatalErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FatalErr {}

fn remote_err(msg: impl Into<String>) -> io::Error {
    io::Error::other(RemoteErr(msg.into()))
}
fn fatal_err(msg: impl Into<String>) -> io::Error {
    io::Error::other(FatalErr(msg.into()))
}
fn is_remote(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|x| x.is::<RemoteErr>())
}
/// 다시 연결해도 소용없는 오류인지 (그 밖의 오류는 연결 끊김으로 보고 이어받기 시도)
fn is_fatal(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::InvalidData || e.get_ref().is_some_and(|x| x.is::<FatalErr>())
}

/// 수신 PC의 증거 폴더를 저장소로 씁니다
pub struct RemoteStore {
    inner: Arc<Inner>,
    location: String,
}

/// 파일마다 보낸 양과 최근 데이터 (이어받기용)
struct Track {
    sent: u64,
    ring: VecDeque<u8>,
}

struct Inner {
    ch: Mutex<Channel>,
    next_id: AtomicU32,
    /// 연결이 깨졌고 이어받기도 할 수 없으면 이후 요청을 바로 실패시킴
    broken: AtomicBool,
    addrs: Vec<SocketAddr>,
    code: String,
    /// 수신 측이 정한 세션 번호 (이어받기 때 같은 세션인지 확인)
    session: String,
    /// 잠금 순서: ch → tracks
    tracks: Mutex<HashMap<u32, Track>>,
    resumes: AtomicU32,
    /// 끊김, 이어받기 기록 (수집 로그에 남김)
    notes: Mutex<Vec<String>>,
    /// 수신 PC가 검증 중: 그동안 오는 진행 알림은 verify가 받으므로 pending()이 가로채면 안 됨
    verifying: AtomicBool,
    /// 수집 중단 요청 (재접속을 기다리는 중에도 바로 멈추게)
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// 마무리 기록 단계 (중단 요청과 상관없이 재접속하되, 오래 기다리지 않음)
    finishing: AtomicBool,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Channel> {
        // 다른 스레드가 패닉해도 연결 상태는 그대로 쓸 수 있음 (오염 무시)
        self.ch.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn tracks(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Track>> {
        self.tracks.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn cancelled(&self) -> bool {
        self.cancel.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|c| c.load(Ordering::Relaxed))
    }

    fn note(&self, s: String) {
        let line = format!("{} {}: {s}", crate::util::fmt_ts(&crate::util::now_local()), crate::tr!("네트워크", "Network"));
        self.notes.lock().unwrap_or_else(|p| p.into_inner()).push(line);
    }

    fn check(&self) -> io::Result<()> {
        if self.broken.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, crate::tr!("수신 PC와의 연결이 끊어졌습니다", "Connection to the receiving PC was lost")));
        }
        Ok(())
    }

    fn give_up(&self, ch: &mut Channel, e: io::Error) -> io::Error {
        self.broken.store(true, Ordering::Relaxed);
        let _ = ch.stream.shutdown(Shutdown::Both);
        e
    }

    /// 수신 측은 요청에만 답하므로, 요청 전에 와 있는 데이터는 오류 통보(디스크 가득, 수신 중지 등)이거나 연결 종료.
    /// 보내기 전에 확인해야 쓰기 실패로 연결이 끊기면서 그 이유가 버려지지 않음
    fn pending(&self, ch: &mut Channel) -> io::Result<()> {
        if self.verifying.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut b = [0u8; 1];
        let _ = ch.stream.set_nonblocking(true);
        let r = ch.stream.peek(&mut b);
        let _ = ch.stream.set_nonblocking(false);
        match r {
            Ok(0) => Err(io::Error::new(io::ErrorKind::ConnectionAborted, crate::tr!("수신 PC가 연결을 끊었습니다", "The receiving PC closed the connection"))),
            Ok(_) => {
                ch.deadline = Some(Instant::now() + Duration::from_secs(2));
                let got = ch.recv();
                ch.deadline = None;
                match got {
                    Ok((K_ERROR, body)) => Err(fatal_err(crate::tr!(format!("수신 PC 오류: {}", String::from_utf8_lossy(&body)), format!("Receiving PC error: {}", String::from_utf8_lossy(&body))))),
                    Ok((k, _)) => Err(fatal_err(crate::tr!(format!("예상하지 않은 메시지 {k}"), format!("Unexpected message {k}")))),
                    Err(e) => Err(e),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 오류 처리: 연결이 끊긴 종류면 다시 연결해 이어받고(Ok), 아니면 연결을 닫고 오류를 돌려줌
    fn recover(&self, ch: &mut Channel, e: io::Error) -> io::Result<()> {
        if is_remote(&e) {
            return Err(e);
        }
        if is_fatal(&e) || self.broken.load(Ordering::Relaxed) || self.session.is_empty() {
            return Err(self.give_up(ch, e));
        }
        self.reconnect(ch, &e)
    }

    fn reconnect(&self, ch: &mut Channel, why: &io::Error) -> io::Result<()> {
        let _ = ch.stream.shutdown(Shutdown::Both);
        let why = if is_timeout(why) { crate::tr!(format!("{}초 동안 응답 없음", IDLE_TIMEOUT.as_secs()), format!("No response for {} s", IDLE_TIMEOUT.as_secs())) } else { why.to_string() };
        self.note(crate::tr!(
            format!("수신 PC와 연결이 끊어졌습니다 ({why}). 다시 연결해 이어받습니다"),
            format!("Connection to the receiving PC was lost ({why}). Reconnecting to resume")
        ));
        let until = Instant::now() + if self.finishing.load(Ordering::Relaxed) { RESUME_TRY_FINISHING } else { RESUME_TRY };
        let mut last;
        let mut refused = 0u32;
        loop {
            if self.broken.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, crate::tr!("수신 PC와의 연결이 끊어졌습니다", "Connection to the receiving PC was lost")));
            }
            if self.cancelled() {
                self.broken.store(true, Ordering::Relaxed);
                let msg = crate::tr!("중단 요청으로 수신 PC 재접속을 멈췄습니다", "Reconnection to the receiving PC stopped by cancel request").to_string();
                self.note(msg.clone());
                return Err(fatal_err(msg));
            }
            let r = self.try_resume();
            // 접속 거부 = 수신 PC는 켜져 있으나 수신 프로그램이 대기하지 않음 (끝났거나 꺼짐).
            // 세션 정보는 수신 프로그램 메모리에만 있으므로 다시 켜도 이어받을 수 없음 → 몇 번 확인 뒤 포기
            refused = if r.as_ref().err().is_some_and(|e| e.kind() == io::ErrorKind::ConnectionRefused) { refused + 1 } else { 0 };
            if refused >= REFUSED_GIVE_UP {
                self.broken.store(true, Ordering::Relaxed);
                let msg = crate::tr!(
                    "수신 PC에서 수신 프로그램이 대기하고 있지 않습니다 (접속 거부). 수신이 끝났거나 꺼져서 이어받을 수 없습니다",
                    "The receiver is not listening on the receiving PC (connection refused). It has finished or been closed, so the transfer cannot be resumed"
                )
                .to_string();
                self.note(msg.clone());
                return Err(fatal_err(msg));
            }
            match r {
                Ok(c) => {
                    *ch = c;
                    let n = self.resumes.fetch_add(1, Ordering::Relaxed) + 1;
                    self.note(crate::tr!(format!("다시 연결해 이어받았습니다 ({n}번째)"), format!("Reconnected and resumed (#{n})")));
                    return Ok(());
                }
                Err(e) if is_fatal(&e) || is_remote(&e) => {
                    self.broken.store(true, Ordering::Relaxed);
                    let msg = crate::tr!(format!("이어받기 실패: {e}"), format!("Resume failed: {e}"));
                    self.note(msg.clone());
                    return Err(fatal_err(msg));
                }
                Err(e) => last = e.to_string(),
            }
            if Instant::now() >= until {
                self.broken.store(true, Ordering::Relaxed);
                let msg = crate::tr!(
                    format!("수신 PC와 연결이 끊긴 뒤 {}분 동안 다시 연결하지 못했습니다 ({last})", RESUME_TRY.as_secs() / 60),
                    format!("Could not reconnect to the receiving PC for {} min after the connection was lost ({last})", RESUME_TRY.as_secs() / 60)
                );
                self.note(msg.clone());
                return Err(fatal_err(msg));
            }
            // 기다리는 동안에도 중단 요청을 바로 받음
            let wake = Instant::now() + RESUME_INTERVAL;
            while Instant::now() < wake && !self.cancelled() {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// 다시 접속해서 세션을 이어받고, 수신 측이 받지 못한 데이터를 보관분에서 다시 보냄
    fn try_resume(&self) -> io::Result<Channel> {
        let mut last = None;
        let mut stream = None;
        for a in &self.addrs {
            // 중단 요청이 늦게 반영되지 않게 짧게 (주소마다 최대 5초)
            match TcpStream::connect_timeout(a, Duration::from_secs(5)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| last.unwrap_or_else(|| io::Error::other(crate::tr!("수신 PC 주소 없음", "No receiving PC address"))))?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(PREAUTH_DEADLINE)).ok();
        let mut c = Channel::handshake(stream, &self.code, true, Some(Instant::now() + PREAUTH_DEADLINE))?;
        c.send_json(K_RESUME, &ResumeMsg { session: self.session.clone() })?;
        c.deadline = Some(Instant::now() + Duration::from_secs(60));
        let st: ResumedMsg = match c.recv() {
            Ok((K_RESUMED, b)) => json(&b)?,
            Ok((K_ERROR, b)) => return Err(remote_err(crate::tr!(
                format!("수신 PC가 이어받기를 거부했습니다: {}", String::from_utf8_lossy(&b)),
                format!("The receiving PC rejected the resume: {}", String::from_utf8_lossy(&b))
            ))),
            Ok((k, _)) => return Err(fatal_err(crate::tr!(format!("예상하지 않은 응답 {k}"), format!("Unexpected response {k}")))),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(fatal_err(crate::tr!(
                    "연결 코드가 맞지 않습니다 (수신 PC가 수신을 새로 시작했을 수 있음)",
                    "Connection code mismatch (the receiving PC may have restarted receiving)"
                )))
            }
            Err(e) => return Err(e),
        };
        c.authenticated();
        let tracks = self.tracks();
        for (id, t) in tracks.iter() {
            let Some(s) = st.files.iter().find(|s| s.id == *id) else {
                return Err(fatal_err(crate::tr!(format!("수신 PC에 파일 번호 {id}가 없습니다"), format!("File ID {id} does not exist on the receiving PC"))));
            };
            if !s.open {
                // 닫기는 처리됐고 응답만 사라진 경우: 닫기 요청을 다시 보내면 크기를 돌려받음
                continue;
            }
            if s.size > t.sent {
                return Err(fatal_err(crate::tr!(
                    format!("수신 PC가 보낸 것보다 많이 받았다고 알립니다 (파일 {id})"),
                    format!("The receiving PC reports more data than was sent (file {id})")
                )));
            }
            let gap = (t.sent - s.size) as usize;
            if gap > t.ring.len() {
                return Err(fatal_err(crate::tr!(
                    format!(
                        "다시 보내야 할 데이터({})가 보관 범위({})보다 커서 이어받을 수 없습니다",
                        format_bytes(gap as u64),
                        format_bytes(t.ring.len() as u64)
                    ),
                    format!(
                        "Cannot resume: data to resend ({}) exceeds the retained buffer ({})",
                        format_bytes(gap as u64),
                        format_bytes(t.ring.len() as u64)
                    )
                )));
            }
            let mut it = t.ring.range(t.ring.len() - gap..).copied();
            let mut buf = Vec::with_capacity(4 + DATA_FRAME);
            loop {
                buf.clear();
                buf.extend_from_slice(&id.to_be_bytes());
                buf.extend(it.by_ref().take(DATA_FRAME));
                if buf.len() == 4 {
                    break;
                }
                c.send(K_DATA, &buf)?;
            }
        }
        Ok(c)
    }

    /// 요청 하나를 보냄 (응답 없는 것). 끊기면 이어받은 뒤 다시 보냄
    fn send_req(&self, ch: &mut Channel, kind: u8, body: &[u8]) -> io::Result<()> {
        loop {
            let r = match self.pending(ch) {
                Ok(()) => ch.send(kind, body),
                Err(e) => Err(e),
            };
            match r {
                Ok(()) => return Ok(()),
                Err(e) => self.recover(ch, e)?,
            }
        }
    }

    /// 요청을 보내고 응답 하나를 받습니다.
    /// - 수신 측 오류 응답은 `Err`이지만 연결은 유지 (예: 같은 이름 파일)
    /// - `retry`: 끊기면 이어받은 뒤 같은 요청을 다시 보냄 (수신 측은 같은 요청이 두 번 와도 결과가 같게 처리)
    fn request(&self, kind: u8, body: &[u8], expect: u8, retry: bool) -> io::Result<Vec<u8>> {
        self.check()?;
        let mut ch = self.lock();
        loop {
            let r = match self.pending(&mut ch) {
                Ok(()) => match ch.send(kind, body) {
                    Ok(()) => ch.recv(),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            match r {
                Ok((k, b)) if k == expect => return Ok(b),
                Ok((K_ERROR, b)) => return Err(remote_err(crate::tr!(format!("수신 PC 오류: {}", String::from_utf8_lossy(&b)), format!("Receiving PC error: {}", String::from_utf8_lossy(&b))))),
                Ok((k, _)) => return Err(self.give_up(&mut ch, fatal_err(crate::tr!(format!("예상하지 않은 응답 {k}"), format!("Unexpected response {k}"))))),
                Err(e) if !retry => return Err(self.give_up(&mut ch, e)),
                Err(e) => self.recover(&mut ch, e)?,
            }
        }
    }
}

/// 쉬는 동안 신호 보내기: 수신 측이 "대상이 사라졌다"고 오판하지 않게 하고, 끊긴 연결을 빨리 알아내 이어받음
fn heartbeat(inner: Weak<Inner>) {
    let mut last = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let Some(i) = inner.upgrade() else { return };
        if i.broken.load(Ordering::Relaxed) {
            return;
        }
        if last.elapsed() < HEARTBEAT {
            continue;
        }
        // 다른 작업이 연결을 쓰는 중이면 그것이 신호 역할을 함
        let Ok(mut ch) = i.ch.try_lock() else { continue };
        last = Instant::now();
        if let Err(e) = ch.send(K_PING, &[]) {
            if i.recover(&mut ch, e).is_err() {
                return;
            }
        }
    }
}

struct RemoteFile {
    inner: Arc<Inner>,
    id: u32,
}

impl StoreFile for RemoteFile {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        // 잠금은 프레임 하나씩만 잡음 (수집 로그 등 다른 쓰기가 오래 기다리지 않게)
        for part in data.chunks(DATA_FRAME) {
            self.inner.check()?;
            let mut body = Vec::with_capacity(4 + part.len());
            body.extend_from_slice(&self.id.to_be_bytes());
            body.extend_from_slice(part);
            let mut ch = self.inner.lock();
            {
                let mut tr = self.inner.tracks();
                let t = tr.get_mut(&self.id).ok_or_else(|| fatal_err(crate::tr!(format!("파일 번호 {} 기록이 없습니다", self.id), format!("No record for file ID {}", self.id))))?;
                t.sent += part.len() as u64;
                t.ring.extend(part);
                let over = t.ring.len().saturating_sub(REPLAY_BUF);
                if over > 0 {
                    t.ring.drain(..over);
                }
            }
            let r = match self.inner.pending(&mut ch) {
                Ok(()) => ch.send(K_DATA, &body),
                Err(e) => Err(e),
            };
            if let Err(e) = r {
                // 검증 중(로그 쓰기)에는 여기서 다시 연결하지 않음: 검증 루프가 소켓에 남은 수신 측 오류(수신 중지 등)를
                // 읽어 이유를 알리거나, 진짜 끊김이면 그쪽에서 이어받음. 이 프레임은 보관분에 있어 그때 함께 다시 보냄
                if self.inner.verifying.load(Ordering::Relaxed) {
                    return Err(e);
                }
                // 이 프레임은 보관분에 들어 있으므로, 다시 연결하면 수신 측이 받은 지점부터 함께 다시 보냄
                self.inner.recover(&mut ch, e)?;
            }
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> io::Result<()> {
        let body = serde_json::to_vec(&IdMsg { id: self.id }).map_err(io::Error::other)?;
        let b = self.inner.request(K_CLOSE, &body, K_CLOSED, true)?;
        let c: ClosedMsg = json(&b)?;
        let sent = self.inner.tracks().remove(&self.id).map(|t| t.sent).unwrap_or(0);
        // 수신 측이 실제로 쓴 바이트 수와 보낸 바이트 수 대조
        if c.size != sent {
            self.inner.broken.store(true, Ordering::Relaxed);
            return Err(io::Error::other(crate::tr!(
                format!("수신 PC에 저장된 크기가 보낸 크기와 다릅니다 (보냄 {sent}, 저장 {})", c.size),
                format!("Size stored on the receiving PC differs from the size sent (sent {sent}, stored {})", c.size)
            )));
        }
        Ok(())
    }
}

impl RemoteStore {
    /// 수신 PC에 접속해 사건 정보를 보내고, 증거 폴더를 받습니다.
    pub fn connect(addr: &str, code: &str, hello: &Hello) -> Result<(RemoteStore, Accept)> {
        let code = normalize_code(code)?;
        let target = with_default_port(addr);
        let addrs: Vec<SocketAddr> = target.to_socket_addrs().with_context(|| crate::tr!(format!("수신 PC 주소를 해석할 수 없습니다: {target}"), format!("Cannot resolve the receiving PC address: {target}")))?.collect();
        let mut last = None;
        let mut stream = None;
        for a in &addrs {
            match TcpStream::connect_timeout(a, Duration::from_secs(10)) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let stream = stream.ok_or_else(|| {
            let why = last.map(|e| e.to_string()).unwrap_or_default();
            anyhow!(
                "{}",
                crate::tr!(
                    format!("수신 PC({target})에 연결할 수 없습니다: {why}. 수신 PC에서 수신을 시작했는지(GUI: 네트워크 수신, CLI: receive), 주소, 포트가 맞는지, 수신 PC 방화벽이 포트를 허용하는지 확인하세요"),
                    format!("Cannot connect to the receiving PC ({target}): {why}. Check that receiving has been started on the receiving PC (GUI: Network Receive, CLI: receive), that the address and port are correct, and that its firewall allows the port")
                )
            )
        })?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(PREAUTH_DEADLINE)).ok();
        let deadline = Some(Instant::now() + PREAUTH_DEADLINE);
        let mut ch = Channel::handshake(stream, &code, true, deadline).map_err(|e| anyhow!("{}", crate::tr!(format!("수신 PC와 연결 설정 실패: {e}"), format!("Connection setup with the receiving PC failed: {e}"))))?;
        ch.send_json(K_HELLO, hello)?;
        // 수신 측은 저장 매체 확인, 폴더 만들기를 하므로 응답 대기는 넉넉히 (절전에서 깨는 USB 등)
        ch.deadline = Some(Instant::now() + Duration::from_secs(60));
        let accept: Accept = match ch.recv() {
            Ok((K_ACCEPT, b)) => json(&b)?,
            Ok((K_ERROR, b)) => return Err(anyhow!("{}", crate::tr!(format!("수신 PC가 거부했습니다: {}", String::from_utf8_lossy(&b)), format!("The receiving PC rejected the request: {}", String::from_utf8_lossy(&b))))),
            Ok((k, _)) => return Err(anyhow!("{}", crate::tr!(format!("예상하지 않은 응답 {k}"), format!("Unexpected response {k}")))),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(anyhow!(crate::tr!(
                    "연결 코드가 맞지 않습니다. 수신 PC 화면의 코드를 다시 확인하세요",
                    "Connection code mismatch. Check the code shown on the receiving PC"
                )))
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted) => {
                return Err(anyhow!(crate::tr!(
                    "수신 PC가 연결을 끊었습니다. 연결 코드가 맞는지 확인하세요",
                    "The receiving PC closed the connection. Check that the connection code is correct"
                )))
            }
            Err(e) => return Err(anyhow!("{}", crate::tr!(format!("수신 PC 응답 없음: {e}"), format!("No response from the receiving PC: {e}")))),
        };
        ch.authenticated();
        let location = format!("{} ({}), {}", accept.evidence_dir, accept.receiver_host, target);
        let inner = Arc::new(Inner {
            ch: Mutex::new(ch),
            next_id: AtomicU32::new(1),
            broken: AtomicBool::new(false),
            addrs,
            code,
            session: accept.session.clone(),
            tracks: Mutex::new(HashMap::new()),
            resumes: AtomicU32::new(0),
            notes: Mutex::new(Vec::new()),
            verifying: AtomicBool::new(false),
            cancel: Mutex::new(None),
            finishing: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new().name("net-heartbeat".into()).spawn(move || heartbeat(weak)).ok();
        Ok((RemoteStore { inner, location }, accept))
    }

    /// 연결이 끊겼다가 이어받은 횟수
    pub fn resumes(&self) -> u32 {
        self.inner.resumes.load(Ordering::Relaxed)
    }
}

impl RemoteStore {
    /// 수집 중단 요청을 연결: 끊겨서 다시 연결을 기다리는 중에도 바로 멈춤
    pub fn set_cancel(&self, cancel: Arc<AtomicBool>) {
        *self.inner.cancel.lock().unwrap_or_else(|p| p.into_inner()) = Some(cancel);
    }
    /// 이미징이 끝난 뒤(마무리 기록 단계)에는 중단 요청이 있어도 재접속함: 로그, 요약, 보고서는 수신 PC에만 남으므로
    /// 마무리 기록 단계 전용: 이미징이 모두 끝난 뒤에만 부름 (재접속 한도는 1분으로 줄어듦)
    pub fn clear_cancel(&self) {
        *self.inner.cancel.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.inner.finishing.store(true, Ordering::Relaxed);
    }
}

impl EvidenceStore for RemoteStore {
    fn location(&self) -> String {
        self.location.clone()
    }

    fn create(&self, name: &str) -> io::Result<Box<dyn StoreFile>> {
        check_file_name(name)?;
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::to_vec(&OpenMsg { id, name: name.to_string() }).map_err(io::Error::other)?;
        self.inner.request(K_OPEN, &body, K_OK, true)?;
        self.inner.tracks().insert(id, Track { sent: 0, ring: VecDeque::new() });
        Ok(Box::new(RemoteFile { inner: self.inner.clone(), id }))
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
        self.inner.check()?;
        // 잠금은 보내기, 받기 순간에만 잡음: 콜백(로그)이 같은 연결로 수집 로그를 쓰기 때문 (잡은 채 부르면 교착)
        log(LogLevel::Info, crate::tr!("수신 PC가 저장한 파일을 다시 읽어 해시를 계산합니다", "The receiving PC is re-reading the stored files to compute hashes"));
        let msg = serde_json::to_vec(&VerifyMsg { files: files.to_vec(), format, algs: algs.to_vec() })?;
        {
            let mut ch = self.inner.lock();
            self.inner.send_req(&mut ch, K_VERIFY, &msg)?;
            // 검증 요청 뒤로는 수신 측이 진행 알림을 보내므로, 로그 쓰기가 그것을 오류로 오인하지 않게 표시
            self.inner.verifying.store(true, Ordering::Relaxed);
        }
        struct Clear<'a>(&'a AtomicBool);
        impl Drop for Clear<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Relaxed);
            }
        }
        let _clear = Clear(&self.inner.verifying);
        // 중단 요청이 와도 연결을 끊지 않고 수신 PC가 검증을 끝낼 때까지 기다림:
        // 끊으면 중단 기록(로그, 요약, 보고서)을 남길 곳이 없어짐 (로그는 수신 PC에만 있음)
        let mut told = false;
        let mut restarts = 0u32;
        loop {
            if cancel.load(Ordering::Relaxed) && !told {
                log(
                    LogLevel::Warn,
                    crate::tr!(
                        "중단 요청: 수신 PC가 검증 읽기를 끝내면 중단하고 기록을 저장합니다",
                        "Cancel requested: will stop and save records once the receiving PC finishes the verification read"
                    ),
                );
                told = true;
            }
            let got = {
                let mut ch = self.inner.lock();
                match ch.recv() {
                    Ok(x) => x,
                    Err(e) => {
                        // 끊기면 이어받은 뒤 검증을 처음부터 다시 요청 (계속 끊기는 망에서 끝없이 반복하지 않게 횟수 제한)
                        self.inner.recover(&mut ch, e)?;
                        restarts += 1;
                        if restarts > MAX_VERIFY_RESTARTS {
                            return Err(anyhow!(
                                "{}",
                                crate::tr!(
                                    format!("검증 중 연결이 {restarts}번 끊겨 검증을 끝내지 못했습니다. 받은 파일은 수신 PC에 있으니 그쪽에서 다시 검증하세요"),
                                    format!("Verification could not be completed: the connection was lost {restarts} times. The received files are on the receiving PC; verify them there")
                                )
                            ));
                        }
                        log(LogLevel::Warn, &crate::tr!(
                                format!("검증 중 연결이 끊겨 이어받은 뒤 검증을 처음부터 다시 합니다 ({restarts}/{MAX_VERIFY_RESTARTS})"),
                                format!("Connection lost during verification; resumed and restarting verification from the beginning ({restarts}/{MAX_VERIFY_RESTARTS})")
                            ));
                        self.inner.send_req(&mut ch, K_VERIFY, &msg)?;
                        continue;
                    }
                }
            };
            match got {
                (K_VPROGRESS, b) => {
                    let p: VProgress = json(&b)?;
                    progress(p.done, p.total);
                }
                (K_VDONE, b) => {
                    let d: VDone = json(&b)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    return Ok(Some((d.hashes, d.done)));
                }
                (K_ERROR, b) => return Err(anyhow!("{}", crate::tr!(format!("수신 PC 검증 실패: {}", String::from_utf8_lossy(&b)), format!("Verification failed on the receiving PC: {}", String::from_utf8_lossy(&b))))),
                (k, _) => return Err(anyhow!("{}", crate::tr!(format!("예상하지 않은 응답 {k}"), format!("Unexpected response {k}")))),
            }
        }
    }

    fn finish_session(&self, status: &str) -> Result<()> {
        let body = serde_json::to_vec(&Bye { status: status.to_string() })?;
        // 종료 알림은 다시 보내지 않음 (수신 측은 받는 즉시 세션을 끝내므로 이어받을 상대가 없음)
        self.inner.request(K_BYE, &body, K_OK, false)?;
        self.inner.broken.store(true, Ordering::Relaxed); // 신호 스레드 종료
        Ok(())
    }

    fn notes(&self) -> Vec<String> {
        self.inner.notes.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Drop for RemoteStore {
    fn drop(&mut self) {
        self.inner.broken.store(true, Ordering::Relaxed);
    }
}

fn with_default_port(addr: &str) -> String {
    let a = addr.trim();
    // "1.2.3.4", "host", "[::1]" → 기본 포트 붙임. "1.2.3.4:7700", "[::1]:7700"은 그대로
    let has_port = if a.starts_with('[') { a.contains("]:") } else { a.matches(':').count() == 1 };
    if has_port {
        a.to_string()
    } else if a.contains(':') && !a.starts_with('[') {
        format!("[{a}]:{DEFAULT_PORT}")
    } else {
        format!("{a}:{DEFAULT_PORT}")
    }
}

// -- 받는 쪽 (분석 PC) -----------------------------------------------------

pub struct ReceiveOptions {
    /// 예: "0.0.0.0:7700"
    pub listen: String,
    pub out_base: PathBuf,
    /// 정규화하지 않은 코드도 됨. 없으면 새로 만듦
    pub code: Option<String>,
    /// 세션 중 연결이 끊겼을 때 이어받기 접속을 기다리는 시간
    pub resume_wait: Duration,
}

/// 이어받기 기본 대기 시간 (보내는 쪽 재시도 10분보다 길게)
pub const DEFAULT_RESUME_WAIT: Duration = Duration::from_secs(15 * 60);

/// 수신 진행 알림 (CLI 출력, GUI "receive" 이벤트)
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RecvEvent {
    Listening { addrs: Vec<String>, code: String },
    /// 코드가 틀린 접속. 다음 접속까지 `delay_secs`초 기다림 (무차별 대입 늦추기)
    #[serde(rename_all = "camelCase")]
    AuthFailed { peer: String, reason: String, count: u32, delay_secs: u64 },
    Rejected { peer: String, reason: String },
    #[serde(rename_all = "camelCase")]
    Accepted { peer: String, case: CaseInfo, host: String, evidence_dir: String, need_bytes: u64 },
    FileOpened { name: String },
    FileClosed { name: String, size: u64 },
    /// 끝까지 받지 못한 파일 (이름 뒤에 .incomplete를 붙여 둠)
    FileIncomplete { name: String, size: u64 },
    #[serde(rename_all = "camelCase")]
    Received { total: u64, need: u64, rate_bps: f64 },
    Verifying { done: u64, total: u64 },
    VerifyDone,
    Finished { status: String },
    /// 연결이 끊김. `wait_secs` 동안 같은 코드로 이어받기 접속을 기다림
    #[serde(rename_all = "camelCase")]
    Disconnected { reason: String, wait_secs: u64 },
    /// 보내는 쪽이 다시 연결해 이어받음
    Resumed { peer: String, count: u32 },
}

/// 수신 중지 제어 (GUI 중지 버튼, CLI Ctrl+C)
#[derive(Default)]
pub struct ReceiveControl {
    stop: Arc<AtomicBool>,
    stream: Mutex<Option<TcpStream>>,
}

impl ReceiveControl {
    /// 중지 요청.
    /// - 대기, 인증 단계: 잡고 있는 연결을 바로 끊음
    /// - 받는 중: 1초 안에 세션이 알아채고, 보내는 쪽에 "수신 PC에서 중지" 이유를 보낸 뒤 끝냄
    ///   (받다 만 파일은 .incomplete로 남음)
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.release();
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<TcpStream>> {
        self.stream.lock().unwrap_or_else(|p| p.into_inner())
    }
    /// 인증 전 연결을 잡아 둠 (중지할 때 끊을 수 있게)
    fn hold(&self, s: &TcpStream) {
        *self.slot() = s.try_clone().ok();
        if self.stopped() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
    /// 잡아 둔 연결을 끊고 놓음 (거부한 연결: 복제 핸들이 남아 연결이 열린 채로 있지 않게)
    fn release(&self) {
        if let Some(s) = self.slot().take() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
    /// 인증된 세션: 복제 핸들만 놓음 (중지는 세션이 직접 처리)
    fn detach(&self) {
        self.slot().take();
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveOutcome {
    pub evidence_dir: PathBuf,
    pub status: String,
    pub files: Vec<(String, u64)>,
    /// 끝까지 받지 못해 `.incomplete`로 이름을 바꾼 파일
    pub incomplete: Vec<(String, u64)>,
    /// 연결이 끊겼다가 이어받은 횟수
    pub resumes: u32,
}

/// 이 PC의 대표 IP 주소 (기본 경로 인터페이스). UDP connect는 패킷을 보내지 않음
pub fn local_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip().to_string())
}

/// 이 PC의 IPv4 주소 모두 (루프백 제외). 기본 경로 주소를 맨 앞에. 수신 화면의 "받을 네트워크" 선택용
pub fn local_ips() -> Vec<String> {
    let mut v: Vec<String> = local_ip().into_iter().collect();
    for ip in interface_ips() {
        if !v.contains(&ip) {
            v.push(ip);
        }
    }
    v
}

#[cfg(unix)]
fn interface_ips() -> Vec<String> {
    let mut out = Vec::new();
    // SAFETY: getifaddrs가 돌려준 목록을 읽기만 하고 freeifaddrs로 돌려줌
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let a = &*p;
            let up = a.ifa_flags & libc::IFF_UP as libc::c_uint != 0;
            let lo = a.ifa_flags & libc::IFF_LOOPBACK as libc::c_uint != 0;
            if up && !lo && !a.ifa_addr.is_null() && (*a.ifa_addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(a.ifa_addr as *const libc::sockaddr_in);
                out.push(std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string());
            }
            p = a.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

/// Windows: 이 PC 이름을 풀면 모든 어댑터 주소가 나옴 (DNS 질의 없이 로컬에서 풂)
#[cfg(not(unix))]
fn interface_ips() -> Vec<String> {
    use std::net::ToSocketAddrs;
    let host = std::env::var("COMPUTERNAME").unwrap_or_default();
    if host.is_empty() {
        return Vec::new();
    }
    (host.as_str(), 0)
        .to_socket_addrs()
        .map(|it| it.filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_link_local() => Some(v4.to_string()),
            _ => None,
        }).collect())
        .unwrap_or_default()
}

/// 저장 폴더 검사 함수 (수신 측 저장 매체의 여유 공간, 파일시스템 확인). 돌려주는 값: (매체 설명, 파일시스템, 여유, 경고)
pub type SpaceCheck = dyn Fn(&Path, u64, u64) -> std::result::Result<(String, String, u64, Option<String>), String>;

/// 수신자가 중지했을 때의 오류 문구 (화면에서 "실패"와 구분)
pub const STOPPED_MSG: &str = "수신을 중지했습니다";

/// 중지 오류 문구 (현재 언어). 한국어일 때는 `STOPPED_MSG`와 같음 (GUI는 늘 한국어라 `STOPPED_MSG`로 비교)
pub fn stopped_msg() -> &'static str {
    crate::tr!(STOPPED_MSG, "Receiving stopped")
}

/// 대상 시스템의 연결을 받아 증거를 저장합니다. 세션 하나를 끝내면 돌아옵니다.
/// 세션 중에 연결이 끊기면 `opts.resume_wait` 동안 같은 코드로 이어받기 접속을 기다립니다.
pub fn serve(opts: &ReceiveOptions, space: &SpaceCheck, events: &mut dyn FnMut(RecvEvent), ctrl: &ReceiveControl) -> Result<ReceiveOutcome> {
    let code = match &opts.code {
        Some(c) => normalize_code(c)?,
        None => normalize_code(&new_code()?)?,
    };
    let listener = TcpListener::bind(&opts.listen).with_context(|| crate::tr!(
            format!("{}에서 대기할 수 없습니다 (다른 프로그램이 포트를 쓰는 중일 수 있음)", opts.listen),
            format!("Cannot listen on {} (another program may be using the port)", opts.listen)
        ))?;
    // 중지 요청을 확인할 수 있게 접속 대기는 짧게 나눠서 함
    listener.set_nonblocking(true)?;
    let la = listener.local_addr()?;
    let mut addrs = Vec::new();
    if la.ip().is_unspecified() {
        for ip in local_ips() {
            addrs.push(format!("{ip}:{}", la.port()));
        }
    } else {
        addrs.push(la.to_string());
    }
    events(RecvEvent::Listening { addrs, code: format!("{}-{}", &code[..5], &code[5..]) });

    let mut failures: u32 = 0;
    loop {
        let Some((mut ch, peer, first)) = accept_one(&listener, &code, events, ctrl, &mut failures)? else { continue };
        let hello: Hello = match first {
            (K_HELLO, b) => match json(&b) {
                Ok(h) => h,
                Err(e) => {
                    let msg = crate::tr!(format!("보내는 쪽 WS Collector 버전이 다릅니다 ({e})"), format!("The sender's WS Collector version differs ({e})"));
                    let _ = ch.send(K_ERROR, msg.as_bytes());
                    events(RecvEvent::Rejected { peer, reason: msg });
                    continue;
                }
            },
            (K_RESUME, _) => {
                let _ = ch.send(K_ERROR, crate::tr!(
                        "이어받을 세션이 없습니다 (수신 PC가 수신을 새로 시작함). 처음부터 다시 보내세요",
                        "No session to resume (the receiving PC restarted receiving). Send again from the beginning"
                    )
                    .as_bytes());
                events(RecvEvent::Rejected { peer, reason: crate::tr!("없는 세션의 이어받기 요청", "Resume request for an unknown session").into() });
                continue;
            }
            _ => {
                events(RecvEvent::Rejected { peer, reason: crate::tr!("첫 메시지가 Hello가 아님", "First message is not Hello").into() });
                continue;
            }
        };
        match accept_session(&mut ch, &hello, &peer, opts, space) {
            Ok((dir, accept)) => {
                events(RecvEvent::Accepted {
                    peer: peer.clone(),
                    case: hello.case.clone(),
                    host: hello.host.clone(),
                    evidence_dir: accept.evidence_dir.clone(),
                    need_bytes: hello.need_bytes,
                });
                ch.authenticated();
                ch.stop = Some(ctrl.stop.clone());
                ctrl.detach();
                let mut sess = Session {
                    id: accept.session,
                    dir,
                    need: hello.need_bytes,
                    open: HashMap::new(),
                    closed: Vec::new(),
                    total: 0,
                    started: Instant::now(),
                    resumes: 0,
                };
                let r = run_session(&mut ch, &listener, &code, &mut sess, opts.resume_wait, events, ctrl, &mut failures);
                let incomplete = mark_incomplete(&mut sess, events);
                // 중지로 끝났으면 오류 문구가 STOPPED_MSG, 그 밖의 진짜 오류는 그대로 돌려줌
                let status = r?;
                return Ok(ReceiveOutcome {
                    evidence_dir: sess.dir.clone(),
                    status,
                    files: sess.closed.iter().map(|(_, n, s)| (n.clone(), *s)).collect(),
                    incomplete,
                    resumes: sess.resumes,
                });
            }
            Err(reason) => {
                let _ = ch.send(K_ERROR, reason.as_bytes());
                events(RecvEvent::Rejected { peer, reason });
                let _ = ch.stream.shutdown(Shutdown::Both);
                continue;
            }
        }
    }
}

/// 접속 하나를 받아 코드를 확인하고 첫 메시지를 읽음.
/// 거부, 대기로 끝나면 `Ok(None)` (호출한 쪽은 다시 기다림), 중지면 `Err(STOPPED_MSG)`
fn accept_one(
    listener: &TcpListener,
    code: &str,
    events: &mut dyn FnMut(RecvEvent),
    ctrl: &ReceiveControl,
    failures: &mut u32,
) -> Result<Option<(Channel, String, (u8, Vec<u8>))>> {
    // 앞에서 거부한 연결 정리
    ctrl.release();
    if ctrl.stopped() {
        return Err(anyhow!(stopped_msg()));
    }
    let (stream, peer) = match listener.accept() {
        Ok(x) => x,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            std::thread::sleep(Duration::from_millis(100));
            return Ok(None);
        }
        Err(e) => return Err(anyhow!("{}", crate::tr!(format!("접속 받기 실패: {e}"), format!("Failed to accept connection: {e}")))),
    };
    stream.set_nonblocking(false)?;
    ctrl.hold(&stream);
    let peer = peer.to_string();
    stream.set_nodelay(true).ok();
    stream.set_write_timeout(Some(PREAUTH_DEADLINE)).ok();
    let deadline = Some(Instant::now() + PREAUTH_DEADLINE);
    let mut ch = match Channel::handshake(stream, code, false, deadline) {
        Ok(c) => c,
        Err(_) if ctrl.stopped() => return Err(anyhow!(stopped_msg())),
        Err(e) => {
            events(RecvEvent::Rejected { peer, reason: e.to_string() });
            return Ok(None);
        }
    };
    // 첫 메시지를 풀 수 있어야 코드가 맞는 것
    match ch.recv() {
        Ok(first) => Ok(Some((ch, peer, first))),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // 수신기를 끝내지 않고 점점 오래 기다림: 50비트 코드를 온라인으로 맞히는 것은 불가능하고,
            // 틀린 접속 몇 번으로 수신기를 종료시키는 방해도 막음
            *failures += 1;
            let delay = u64::from((*failures).min(10));
            events(RecvEvent::AuthFailed { peer, reason: e.to_string(), count: *failures, delay_secs: delay });
            let _ = ch.stream.shutdown(Shutdown::Both);
            let until = Instant::now() + Duration::from_secs(delay);
            while Instant::now() < until && !ctrl.stopped() {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None)
        }
        Err(_) if ctrl.stopped() => Err(anyhow!(stopped_msg())),
        Err(e) => {
            events(RecvEvent::Rejected { peer, reason: e.to_string() });
            Ok(None)
        }
    }
}

fn accept_session(ch: &mut Channel, h: &Hello, peer: &str, opts: &ReceiveOptions, space: &SpaceCheck) -> std::result::Result<(PathBuf, Accept), String> {
    if h.protocol != PROTOCOL {
        return Err(crate::tr!(
            format!("프로토콜 버전이 다릅니다 (보내는 쪽 {}, 받는 쪽 {PROTOCOL}). 같은 버전의 WS Collector를 쓰세요", h.protocol),
            format!("Protocol version mismatch (sender {}, receiver {PROTOCOL}). Use the same version of WS Collector", h.protocol)
        ));
    }
    h.case.validate().map_err(|e| e.to_string())?;
    let (media, fs, free, warning) = space(&opts.out_base, h.need_bytes, h.split_bytes)?;
    let dir = h.case.evidence_dir(&opts.out_base);
    if dir.exists() && std::fs::read_dir(&dir).map(|mut r| r.next().is_some()).unwrap_or(false) {
        return Err(crate::tr!(
            format!("이미 파일이 있는 증거 폴더입니다: {}. 증거 번호를 바꾸세요", dir.display()),
            format!("The evidence folder already contains files: {}. Change the evidence number", dir.display())
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| crate::tr!(
            format!("증거 폴더를 만들지 못했습니다 ({}): {e}", dir.display()),
            format!("Failed to create the evidence folder ({}): {e}", dir.display())
        ))?;
    let mut sid = [0u8; 16];
    getrandom::getrandom(&mut sid).map_err(|e| crate::tr!(format!("난수를 만들 수 없습니다: {e}"), format!("Cannot generate random numbers: {e}")))?;
    let accept = Accept {
        evidence_dir: dir.display().to_string(),
        media,
        fs,
        free,
        receiver_host: crate::platform::host_info().hostname,
        peer: peer.to_string(),
        warning,
        session: sid.iter().map(|b| format!("{b:02x}")).collect(),
    };
    ch.send_json(K_ACCEPT, &accept).map_err(|e| e.to_string())?;
    Ok((dir, accept))
}

/// 받는 중인 파일
struct OpenFile {
    name: String,
    w: BufWriter<File>,
    size: u64,
    unsynced: u64,
}

/// 수신 세션 상태 (연결이 끊겨도 유지되어 이어받기에 씀)
struct Session {
    id: String,
    dir: PathBuf,
    need: u64,
    open: HashMap<u32, OpenFile>,
    /// (파일 번호, 이름, 크기)
    closed: Vec<(u32, String, u64)>,
    total: u64,
    started: Instant,
    resumes: u32,
}

/// 세션 처리가 멈춘 이유
enum End {
    /// 연결이 끊김 → 이어받기를 기다림
    Lost(String),
    /// 세션을 끝냄 (오류, 중지)
    Fatal(anyhow::Error),
}

fn lost(e: io::Error) -> End {
    End::Lost(e.to_string())
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    ch: &mut Channel,
    listener: &TcpListener,
    code: &str,
    sess: &mut Session,
    resume_wait: Duration,
    events: &mut dyn FnMut(RecvEvent),
    ctrl: &ReceiveControl,
    failures: &mut u32,
) -> Result<String> {
    loop {
        match session_loop(ch, sess, events, ctrl) {
            Ok(status) => return Ok(status),
            Err(End::Fatal(e)) => return Err(e),
            Err(End::Lost(reason)) => {
                // 받은 데이터는 저장 매체로 내려 둠 (이어받지 못해도 받은 만큼은 남게)
                // 내리지 못하면 받은 크기(f.size)를 믿을 수 없으므로 이어받지 않고 끝냄 (파일은 .incomplete로 남음)
                for f in sess.open.values_mut() {
                    if let Err(e) = f.w.flush() {
                        return Err(anyhow!(
                            "{}",
                            crate::tr!(
                                format!("{} 저장 실패: {e} (연결 끊김: {reason})", f.name),
                                format!("Failed to save {}: {e} (connection lost: {reason})", f.name)
                            )
                        ));
                    }
                }
                let _ = ch.stream.shutdown(Shutdown::Both);
                events(RecvEvent::Disconnected { reason: reason.clone(), wait_secs: resume_wait.as_secs() });
                *ch = wait_resume(listener, code, sess, resume_wait, &reason, events, ctrl, failures)?;
            }
        }
    }
}

/// 끊긴 세션의 이어받기 접속을 기다림. 같은 세션 번호로 오면 각 파일의 받은 크기를 알려 줌
#[allow(clippy::too_many_arguments)]
fn wait_resume(
    listener: &TcpListener,
    code: &str,
    sess: &mut Session,
    resume_wait: Duration,
    reason: &str,
    events: &mut dyn FnMut(RecvEvent),
    ctrl: &ReceiveControl,
    failures: &mut u32,
) -> Result<Channel> {
    let until = Instant::now() + resume_wait;
    loop {
        if Instant::now() >= until {
            return Err(anyhow!(
                "{}",
                crate::tr!(
                    format!("대상 시스템과의 연결이 끊어졌고({reason}), {}초 동안 다시 연결되지 않았습니다", resume_wait.as_secs()),
                    format!("The connection to the target system was lost ({reason}) and was not re-established within {} s", resume_wait.as_secs())
                )
            ));
        }
        let Some((mut c, peer, first)) = accept_one(listener, code, events, ctrl, failures)? else { continue };
        match first {
            (K_RESUME, b) => {
                let m: ResumeMsg = match json(&b) {
                    Ok(m) => m,
                    Err(e) => {
                        events(RecvEvent::Rejected { peer, reason: e.to_string() });
                        continue;
                    }
                };
                if m.session != sess.id {
                    let _ = c.send(K_ERROR, crate::tr!("다른 세션의 이어받기 요청입니다", "Resume request for a different session").as_bytes());
                    events(RecvEvent::Rejected { peer, reason: crate::tr!("다른 세션의 이어받기 요청", "Resume request for a different session").into() });
                    continue;
                }
                let files: Vec<FileState> = sess
                    .open
                    .iter()
                    .map(|(id, f)| FileState { id: *id, size: f.size, open: true })
                    .chain(sess.closed.iter().map(|(id, _, size)| FileState { id: *id, size: *size, open: false }))
                    .collect();
                if let Err(e) = c.send_json(K_RESUMED, &ResumedMsg { files }) {
                    events(RecvEvent::Rejected { peer, reason: e.to_string() });
                    continue;
                }
                c.authenticated();
                c.stop = Some(ctrl.stop.clone());
                ctrl.detach();
                sess.resumes += 1;
                events(RecvEvent::Resumed { peer, count: sess.resumes });
                return Ok(c);
            }
            (K_HELLO, _) => {
                let _ = c.send(K_ERROR, crate::tr!(
                        "이 수신기는 끊긴 세션의 이어받기를 기다리는 중이라 새 수집을 받을 수 없습니다",
                        "This receiver is waiting to resume an interrupted session and cannot accept a new collection"
                    )
                    .as_bytes());
                events(RecvEvent::Rejected { peer, reason: crate::tr!("이어받기 대기 중에 새 수집 요청", "New collection request while waiting to resume").into() });
            }
            _ => events(RecvEvent::Rejected { peer, reason: crate::tr!("첫 메시지가 이어받기가 아님", "First message is not a resume request").into() }),
        }
    }
}

/// 끝까지 받지 못한 파일은 정상 증거 파일처럼 보이지 않게 이름을 바꿔 둠
fn mark_incomplete(sess: &mut Session, events: &mut dyn FnMut(RecvEvent)) -> Vec<(String, u64)> {
    let mut incomplete = Vec::new();
    for (_, mut f) in sess.open.drain() {
        let _ = f.w.flush();
        if let Ok(file) = f.w.into_inner() {
            let _ = file.sync_all();
        }
        let from = sess.dir.join(&f.name);
        let to = sess.dir.join(format!("{}.incomplete", f.name));
        let name = if std::fs::rename(&from, &to).is_ok() { format!("{}.incomplete", f.name) } else { f.name };
        events(RecvEvent::FileIncomplete { name: name.clone(), size: f.size });
        incomplete.push((name, f.size));
    }
    incomplete
}

/// 보내는 쪽에 끝내는 이유를 알리고 연결을 닫음.
/// 바로 끊으면(읽지 않은 데이터가 남은 채 close) RST가 가서 보내는 쪽이 이유를 읽기 전에 버려지므로,
/// 쓰기 방향만 닫고 남은 데이터를 잠깐 비운 뒤 끊음
fn close_with_reason(ch: &mut Channel, msg: &str) {
    let _ = ch.send(K_ERROR, msg.as_bytes());
    let _ = ch.stream.shutdown(Shutdown::Write);
    let _ = ch.stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut sink = vec![0u8; 64 << 10];
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        match ch.stream.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(_) => break,
        }
    }
    let _ = ch.stream.shutdown(Shutdown::Both);
}

/// 세션 메시지 처리. 정상 종료(BYE)면 상태 문자열을 돌려줌
fn session_loop(ch: &mut Channel, sess: &mut Session, events: &mut dyn FnMut(RecvEvent), ctrl: &ReceiveControl) -> std::result::Result<String, End> {
    let need = sess.need;
    let limit = need.saturating_add(RECEIVE_SLACK).saturating_add(need / 100);
    let mut last = Instant::now();
    let mut last_total = sess.total;

    // 세션을 끝내야 하는 오류: 보내는 쪽에 이유를 알리고, 보내는 쪽이 그 이유를 읽을 수 있게
    // 쓰기 방향만 닫은 뒤 남은 데이터를 잠깐 비우고 끊음 (바로 끊으면 RST로 이유가 사라짐)
    macro_rules! bail {
        ($($t:tt)*) => {{
            let msg = format!($($t)*);
            close_with_reason(ch, &msg);
            return Err(End::Fatal(anyhow!(msg)));
        }};
    }

    loop {
        let (kind, body) = match ch.recv() {
            Ok(x) => x,
            Err(_) if ctrl.stopped() => {
                close_with_reason(ch, crate::tr!("수신 PC에서 수신을 중지했습니다", "Receiving was stopped on the receiving PC"));
                return Err(End::Fatal(anyhow!(stopped_msg())));
            }
            Err(e) if is_timeout(&e) => return Err(End::Lost(crate::tr!(format!("{}초 동안 응답 없음", IDLE_TIMEOUT.as_secs()), format!("No response for {} s", IDLE_TIMEOUT.as_secs())))),
            Err(e) => return Err(lost(e)),
        };
        match kind {
            K_PING => {}
            K_OPEN => {
                let m: OpenMsg = match json(&body) {
                    Ok(m) => m,
                    Err(e) => bail!("{e}"),
                };
                if let Some(f) = sess.open.get(&m.id) {
                    // 이어받기 후 다시 온 요청 (처리는 됐고 응답만 사라짐)
                    if f.name == m.name {
                        ch.send(K_OK, &[]).map_err(lost)?;
                        continue;
                    }
                    bail!("{}", crate::tr!(format!("이미 열린 파일 번호 {}", m.id), format!("File ID {} is already open", m.id)));
                }
                if sess.open.len() >= MAX_OPEN_FILES {
                    bail!("{}", crate::tr!(format!("동시에 열 수 있는 파일 수({MAX_OPEN_FILES})를 넘었습니다"), format!("Exceeded the maximum number of open files ({MAX_OPEN_FILES})")));
                }
                // 파일 하나를 만들지 못한 것은 세션을 끝내지 않음 (보내는 쪽이 로그, 보고서를 이어서 보낼 수 있게)
                if let Err(e) = check_file_name(&m.name) {
                    ch.send(K_ERROR, e.to_string().as_bytes()).map_err(lost)?;
                    continue;
                }
                match std::fs::OpenOptions::new().write(true).create_new(true).open(sess.dir.join(&m.name)) {
                    Ok(f) => {
                        sess.open.insert(m.id, OpenFile { name: m.name.clone(), w: BufWriter::with_capacity(1 << 20, f), size: 0, unsynced: 0 });
                        events(RecvEvent::FileOpened { name: m.name });
                        ch.send(K_OK, &[]).map_err(lost)?;
                    }
                    Err(e) => ch.send(K_ERROR, crate::tr!(format!("{} 만들기 실패: {e}", m.name), format!("Failed to create {}: {e}", m.name)).as_bytes()).map_err(lost)?,
                }
            }
            K_DATA => {
                if body.len() < 4 {
                    bail!("{}", crate::tr!("잘못된 데이터 프레임", "Invalid data frame"));
                }
                let id = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                let n = (body.len() - 4) as u64;
                sess.total += n;
                if sess.total > limit {
                    bail!(
                        "{}",
                        crate::tr!(
                            format!("알린 크기({})보다 많은 데이터를 받아 중단합니다 (저장 매체 보호)", format_bytes(need)),
                            format!("Received more data than the announced size ({}); stopping (output media protection)", format_bytes(need))
                        )
                    );
                }
                let Some(f) = sess.open.get_mut(&id) else { bail!("{}", crate::tr!(format!("열리지 않은 파일 번호 {id}"), format!("File ID {id} is not open"))) };
                if let Err(e) = f.w.write_all(&body[4..]) {
                    bail!("{}", crate::tr!(format!("{} 쓰기 실패: {e}", f.name), format!("Failed to write {}: {e}", f.name)));
                }
                f.size += n;
                f.unsynced += n;
                if f.unsynced >= SYNC_EVERY {
                    let r = f.w.flush().and_then(|_| f.w.get_ref().sync_data());
                    if let Err(e) = r {
                        bail!("{}", crate::tr!(format!("{} 저장 실패: {e}", f.name), format!("Failed to save {}: {e}", f.name)));
                    }
                    f.unsynced = 0;
                }
                if last.elapsed() >= Duration::from_millis(500) {
                    let rate = (sess.total - last_total) as f64 / last.elapsed().as_secs_f64();
                    events(RecvEvent::Received { total: sess.total, need, rate_bps: rate });
                    last = Instant::now();
                    last_total = sess.total;
                }
            }
            K_CLOSE => {
                let m: IdMsg = match json(&body) {
                    Ok(m) => m,
                    Err(e) => bail!("{e}"),
                };
                let Some(f) = sess.open.remove(&m.id) else {
                    // 이어받기 후 다시 온 닫기 요청: 이미 닫힌 파일이면 그 크기를 다시 알려 줌
                    if let Some((_, _, size)) = sess.closed.iter().find(|(id, _, _)| *id == m.id) {
                        ch.send_json(K_CLOSED, &ClosedMsg { size: *size }).map_err(lost)?;
                        continue;
                    }
                    bail!("{}", crate::tr!(format!("열리지 않은 파일 번호 {}", m.id), format!("File ID {} is not open", m.id)))
                };
                let (name, size) = (f.name.clone(), f.size);
                let r = f.w.into_inner().map_err(|e| e.into_error()).and_then(|file| file.sync_all());
                if let Err(e) = r {
                    // 목록에서 이미 빠졌으므로 여기서 직접 .incomplete로 바꿈 (잘린 파일이 정상 이름으로 남지 않게)
                    let to = format!("{name}.incomplete");
                    let shown = if std::fs::rename(sess.dir.join(&name), sess.dir.join(&to)).is_ok() { to } else { name.clone() };
                    events(RecvEvent::FileIncomplete { name: shown, size });
                    bail!("{}", crate::tr!(format!("{name} 저장 실패: {e}"), format!("Failed to save {name}: {e}")));
                }
                events(RecvEvent::FileClosed { name: name.clone(), size });
                sess.closed.push((m.id, name, size));
                ch.send_json(K_CLOSED, &ClosedMsg { size }).map_err(lost)?;
            }
            K_VERIFY => {
                let m: VerifyMsg = match json(&body) {
                    Ok(m) => m,
                    Err(e) => bail!("{e}"),
                };
                // 검증 중에는 연결을 이 함수가 독점하므로 진행 알림을 바로 보냄
                let stop = AtomicBool::new(false);
                let mut send_err: Option<io::Error> = None;
                let dir = sess.dir.clone();
                let r = {
                    let ch_ref = &mut *ch;
                    let mut on_progress = |done: u64, t: u64| {
                        if ctrl.stopped() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        if send_err.is_none() {
                            if let Err(e) = ch_ref.send_json(K_VPROGRESS, &VProgress { done, total: t }) {
                                send_err = Some(e);
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                        events(RecvEvent::Verifying { done, total: t });
                    };
                    let mut on_log = |_l: LogLevel, _m: &str| {};
                    verify_dir(&dir, &m.files, m.format, &m.algs, &stop, &mut on_progress, &mut on_log)
                };
                if ctrl.stopped() {
                    close_with_reason(ch, crate::tr!("수신 PC에서 수신을 중지했습니다", "Receiving was stopped on the receiving PC"));
                    return Err(End::Fatal(anyhow!(stopped_msg())));
                }
                if let Some(e) = send_err {
                    // 보내는 쪽이 이어받은 뒤 검증을 다시 요청함
                    return Err(lost(e));
                }
                match r {
                    Ok(Some((hashes, done))) => {
                        events(RecvEvent::VerifyDone);
                        ch.send_json(K_VDONE, &VDone { hashes, done }).map_err(lost)?;
                    }
                    Ok(None) => return Err(End::Lost(crate::tr!("검증 중 연결 끊김", "Connection lost during verification").into())),
                    // 검증 실패는 세션을 끝내지 않음 (보내는 쪽이 요약, 보고서를 이어서 보냄)
                    Err(e) => ch.send(K_ERROR, format!("{e:#}").as_bytes()).map_err(lost)?,
                }
            }
            K_BYE => {
                let b: Bye = match json(&body) {
                    Ok(b) => b,
                    Err(e) => bail!("{e}"),
                };
                let _ = ch.send(K_OK, &[]);
                let secs = sess.started.elapsed().as_secs();
                let resumed = if sess.resumes > 0 { crate::tr!(format!(", 이어받기 {}번", sess.resumes), format!(", resumed {} times", sess.resumes)) } else { String::new() };
                events(RecvEvent::Finished { status: crate::tr!(
                    format!("{} (받은 데이터 {}, {}초{resumed})", b.status, format_bytes(sess.total), secs),
                    format!("{} (received {}, {} s{resumed})", crate::i18n::status_label(&b.status), format_bytes(sess.total), secs)
                ) });
                return Ok(b.status);
            }
            k => bail!("{}", crate::tr!(format!("알 수 없는 메시지 {k}"), format!("Unknown message {k}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_format() {
        let c = new_code().unwrap();
        assert_eq!(c.len(), 11);
        assert_eq!(&c[5..6], "-");
        assert_eq!(normalize_code(&c.to_lowercase()).unwrap(), c.replace('-', ""));
        assert!(normalize_code("ABCDE-FGHI0").is_err()); // 0은 쓰지 않음
        assert!(normalize_code("ABC").is_err());
    }

    #[test]
    fn default_port() {
        assert_eq!(with_default_port("10.0.0.5"), "10.0.0.5:7700");
        assert_eq!(with_default_port("10.0.0.5:9000"), "10.0.0.5:9000");
        assert_eq!(with_default_port("::1"), "[::1]:7700");
        assert_eq!(with_default_port("[::1]:9000"), "[::1]:9000");
    }

    struct Mem(Vec<u8>);
    impl crate::source::Source for Mem {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            let s = off as usize;
            let n = buf.len().min(self.0.len().saturating_sub(s));
            buf[..n].copy_from_slice(&self.0[s..s + n]);
            Ok(n)
        }
    }
    /// 실제 수집처럼 로그를 같은 연결의 수신 측 로그 파일에 씀 (검증 중 교착 재현용)
    struct LogToStore(Box<dyn StoreFile>);
    impl crate::imaging::Observer for LogToStore {
        fn progress(&mut self, _: &crate::imaging::Progress) {}
        fn log(&mut self, _: LogLevel, msg: &str) {
            self.0.write_all(msg.as_bytes()).unwrap();
            self.0.write_all(b"\n").unwrap();
        }
    }

    fn case(ev: &str) -> CaseInfo {
        CaseInfo { case_number: "2026-NET-1".into(), evidence_number: ev.into(), examiner: "홍길동".into(), organization: String::new(), notes: String::new() }
    }

    fn start_receiver(out: PathBuf) -> (u16, std::thread::JoinHandle<Result<ReceiveOutcome>>) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let opts = ReceiveOptions { listen: format!("127.0.0.1:{port}"), out_base: out, code: Some("AAAAA-BBBBB".into()), resume_wait: Duration::from_secs(2) };
        let h = std::thread::spawn(move || {
            let space = |_: &Path, _: u64, _: u64| Ok(("시험 매체".to_string(), "ext4".to_string(), u64::MAX, None));
            serve(&opts, &space, &mut |_| {}, &ReceiveControl::default())
        });
        (port, h)
    }

    fn connect_retry(port: u16, code: &str, ev: &str, need: u64) -> Result<(RemoteStore, Accept)> {
        let hello = Hello {
            protocol: PROTOCOL,
            tool_version: "test".into(),
            case: case(ev),
            host: "TESTHOST".into(),
            interface: "CLI".into(),
            need_bytes: need,
            split_bytes: 0,
        };
        for _ in 0..100 {
            match RemoteStore::connect(&format!("127.0.0.1:{port}"), code, &hello) {
                Err(e) if e.to_string().contains("연결할 수 없습니다") => std::thread::sleep(Duration::from_millis(30)),
                r => return r,
            }
        }
        Err(anyhow!("수신기 시작 대기 시간 초과"))
    }

    fn random(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn cancel_during_verify_keeps_connection_for_log() {
        // 검증 중 중단 요청 → 로그 쓰기가 수신 측 진행 알림을 오류로 오인해 연결을 끊으면 안 됨
        let tmp = tempfile::tempdir().unwrap();
        let (port, srv) = start_receiver(tmp.path().to_path_buf());
        let data = random(48 << 20, 3);
        let (store, _) = connect_retry(port, "aaaaabbbbb", "EV-V", data.len() as u64 + 4096).unwrap();
        let mut f = store.create("EV-V_sda.dd").unwrap();
        f.write_all(&data).unwrap();
        f.finish().unwrap();
        let log = std::cell::RefCell::new(store.create("EV-V_collection.log").unwrap());
        let cancel = AtomicBool::new(true);
        let files = vec![OutputFile { name: "EV-V_sda.dd".into(), size: data.len() as u64 }];
        let mut prog = |_: u64, _: u64| {};
        let mut on_log = |_: LogLevel, m: &str| {
            // 수신 측 진행 알림이 소켓에 쌓일 시간을 줌
            std::thread::sleep(Duration::from_millis(300));
            log.borrow_mut().write_all(m.as_bytes()).unwrap();
        };
        let r = store.verify(&files, ImageFormat::Raw, &[HashAlg::Sha256], &cancel, &mut prog, &mut on_log).unwrap();
        assert!(r.is_none(), "중단 요청이면 결과 없음");
        log.into_inner().finish().expect("검증 뒤에도 같은 연결로 로그를 닫을 수 있어야 함");
        store.finish_session("중단").unwrap();
        let o = srv.join().unwrap().unwrap();
        assert_eq!(o.status, "중단");
        let saved = std::fs::read_to_string(tmp.path().join("2026-NET-1").join("EV-V").join("EV-V_collection.log")).unwrap();
        assert!(saved.contains("중단 요청"));
    }

    #[test]
    fn image_over_network_matches_and_verifies() {
        use crate::imaging::{run_image_to, ImageSpec};
        let tmp = tempfile::tempdir().unwrap();
        let (port, srv) = start_receiver(tmp.path().to_path_buf());
        let data = random((5 << 20) + 4096, 7);
        let (store, acc) = connect_retry(port, "aaaaabbbbb", "EV-1", data.len() as u64).unwrap();
        assert!(acc.evidence_dir.ends_with("EV-1"));
        let store: Arc<dyn EvidenceStore> = Arc::new(store);

        let spec = ImageSpec {
            out_dir: PathBuf::new(),
            base_name: "EV-1_HOST_20261106_sda".into(),
            split_bytes: 2 << 20,
            algs: HashAlg::ALL.to_vec(),
            verify: true,
            zero_fill_errors: true,
            max_bytes: None,
            ext: None,
            format: ImageFormat::Raw,
            compression: Default::default(),
            meta: Default::default(),
        };
        let mut obs = LogToStore(store.create("EV-1_collection.log").unwrap());
        let r = run_image_to(&mut Mem(data.clone()), &spec, store.clone(), &AtomicBool::new(false), &mut obs).unwrap();
        obs.0.finish().unwrap();
        assert_eq!(r.files.len(), 3);
        assert!(r.verify.as_ref().unwrap().matched, "수신 측 재검증 해시가 원본과 같아야 함");

        // 같은 이름 다시 만들기는 거부되지만 세션은 계속됨 (보고서 등을 이어서 보낼 수 있어야 함)
        let mut f = store.create("EV-1_report.html").unwrap();
        f.write_all(b"<html></html>").unwrap();
        f.finish().unwrap();
        assert!(store.create("EV-1_report.html").is_err());
        let mut f = store.create("EV-1_summary.json").unwrap();
        f.write_all(b"{}").unwrap();
        f.finish().unwrap();
        store.finish_session("완료").unwrap();

        let o = srv.join().unwrap().unwrap();
        assert_eq!(o.status, "완료");
        assert!(o.incomplete.is_empty());
        let dir = tmp.path().join("2026-NET-1").join("EV-1");
        let got: Vec<u8> = r.files.iter().flat_map(|f| std::fs::read(dir.join(&f.name)).unwrap()).collect();
        assert_eq!(got, data);
        assert_eq!(std::fs::read(dir.join("EV-1_report.html")).unwrap(), b"<html></html>");
        let log = std::fs::read_to_string(dir.join("EV-1_collection.log")).unwrap();
        assert!(log.contains("검증 일치"), "{log}");
    }

    /// E01을 수신 PC로 보내면 수신 PC가 E01을 풀어서 검증 (RAW처럼 파일 바이트를 해시하면 불일치)
    #[test]
    fn e01_over_network_verifies_on_receiver() {
        use crate::imaging::{run_image_to, ImageSpec};
        let tmp = tempfile::tempdir().unwrap();
        let (port, srv) = start_receiver(tmp.path().to_path_buf());
        let mut data = random(6 << 20, 9);
        data[..(2 << 20)].fill(0);
        let (store, _) = connect_retry(port, "aaaaabbbbb", "EV-E", data.len() as u64).unwrap();
        let store: Arc<dyn EvidenceStore> = Arc::new(store);
        let spec = ImageSpec {
            out_dir: PathBuf::new(),
            base_name: "EV-E_HOST_20261106_sda".into(),
            split_bytes: 0,
            algs: HashAlg::ALL.to_vec(),
            verify: true,
            zero_fill_errors: true,
            max_bytes: None,
            ext: None,
            format: ImageFormat::E01,
            compression: Default::default(),
            meta: Default::default(),
        };
        let mut obs = LogToStore(store.create("EV-E_collection.log").unwrap());
        let r = run_image_to(&mut Mem(data.clone()), &spec, store.clone(), &AtomicBool::new(false), &mut obs).unwrap();
        obs.0.finish().unwrap();
        assert_eq!(r.files.len(), 1);
        assert!(r.files[0].name.ends_with(".E01"));
        assert!(r.stored_bytes < data.len() as u64);
        assert!(r.verify.as_ref().unwrap().matched, "{:?}", r.verify);
        store.finish_session("완료").unwrap();
        srv.join().unwrap().unwrap();
        let dir = tmp.path().join("2026-NET-1").join("EV-E");
        let (h, n) = crate::e01::verify_e01(&dir, &r.files, &HashAlg::ALL, &AtomicBool::new(false), &mut |_, _| {}, &mut |_, _| {}).unwrap().unwrap();
        assert_eq!(n, data.len() as u64);
        assert_eq!(h, r.hashes);
    }

    #[test]
    fn session_end_and_name_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let (port, srv) = start_receiver(tmp.path().to_path_buf());
        // 틀린 코드는 알아볼 수 있는 오류이고, 수신기는 끝나지 않음
        let e = connect_retry(port, "AAAAA-CCCCC", "EV-2", 1 << 20).err().unwrap();
        assert!(e.to_string().contains("연결 코드"), "{e}");
        let (store, _) = connect_retry(port, "AAAAA-BBBBB", "EV-2", 1 << 20).unwrap();
        // 폴더 밖으로 나가는 이름, Windows 장치 이름은 보내기 전에 거부
        assert!(store.create("../x").is_err());
        assert!(store.create("NUL.dd").is_err());
        assert!(store.create("a.").is_err());
        let mut f = store.create("a.txt").unwrap();
        f.write_all(b"abc").unwrap();
        f.finish().unwrap();
        store.finish_session("완료").unwrap();
        let o = srv.join().unwrap().unwrap();
        assert_eq!(o.status, "완료");
        assert_eq!(o.files, vec![("a.txt".to_string(), 3)]);
    }

    #[test]
    fn broken_transfer_marks_partial_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (port, srv) = start_receiver(tmp.path().to_path_buf());
        let (store, _) = connect_retry(port, "AAAAA-BBBBB", "EV-3", 1 << 20).unwrap();
        let mut f = store.create("EV-3_sda.001").unwrap();
        f.write_all(&[7u8; 1000]).unwrap();
        // 닫지 않고 연결을 끊음 (보내는 쪽 비정상 종료)
        drop(f);
        drop(store);
        let e = srv.join().unwrap().err().unwrap();
        assert!(e.to_string().contains("연결"), "{e}");
        let dir = tmp.path().join("2026-NET-1").join("EV-3");
        assert!(!dir.join("EV-3_sda.001").exists(), "끝나지 않은 파일이 정상 이름으로 남으면 안 됨");
        assert_eq!(std::fs::read(dir.join("EV-3_sda.001.incomplete")).unwrap().len(), 1000);
    }

    #[test]
    fn receiver_refuses_more_than_announced() {
        let tmp = tempfile::tempdir().unwrap();
        let (port, srv) = start_receiver(tmp.path().to_path_buf());
        // 1MB만 보낸다고 알리고 그보다 훨씬 많이 보냄 → 수신 측이 끊음 (저장 매체 보호)
        let (store, _) = connect_retry(port, "AAAAA-BBBBB", "EV-4", 1 << 20).unwrap();
        let mut f = store.create("EV-4_sda.dd").unwrap();
        let chunk = vec![1u8; 4 << 20];
        let mut err = None;
        for _ in 0..40 {
            if let Err(e) = f.write_all(&chunk) {
                err = Some(e);
                break;
            }
        }
        let err = err.or_else(|| f.finish().err()).expect("초과 전송은 실패해야 함");
        assert!(err.to_string().contains("많은 데이터") || err.to_string().contains("끊"), "{err}");
        let e = srv.join().unwrap().err().unwrap();
        assert!(e.to_string().contains("많은 데이터"), "{e}");
    }

    #[test]
    fn stop_while_listening_and_while_receiving() {
        // 대기 중 중지
        let tmp = tempfile::tempdir().unwrap();
        let ctrl = Arc::new(ReceiveControl::default());
        let c2 = ctrl.clone();
        let out = tmp.path().to_path_buf();
        let h = std::thread::spawn(move || {
            let opts = ReceiveOptions { listen: "127.0.0.1:0".into(), out_base: out, code: Some("AAAAA-BBBBB".into()), resume_wait: Duration::from_secs(2) };
            let space = |_: &Path, _: u64, _: u64| Ok(("시험 매체".to_string(), "ext4".to_string(), u64::MAX, None));
            serve(&opts, &space, &mut |_| {}, &c2)
        });
        std::thread::sleep(Duration::from_millis(300));
        ctrl.stop();
        let e = h.join().unwrap().err().unwrap();
        assert_eq!(e.to_string(), STOPPED_MSG);

        // 받는 중 중지 → 받다 만 파일은 .incomplete
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let ctrl = Arc::new(ReceiveControl::default());
        let c2 = ctrl.clone();
        let out = tmp.path().to_path_buf();
        let h = std::thread::spawn(move || {
            let opts = ReceiveOptions { listen: format!("127.0.0.1:{port}"), out_base: out, code: Some("AAAAA-BBBBB".into()), resume_wait: Duration::from_secs(2) };
            let space = |_: &Path, _: u64, _: u64| Ok(("시험 매체".to_string(), "ext4".to_string(), u64::MAX, None));
            serve(&opts, &space, &mut |_| {}, &c2)
        });
        let (store, _) = connect_retry(port, "AAAAA-BBBBB", "EV-9", 1 << 20).unwrap();
        let mut f = store.create("EV-9_sda.001").unwrap();
        f.write_all(&[1u8; 5000]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        ctrl.stop();
        let e = h.join().unwrap().err().unwrap();
        assert_eq!(e.to_string(), STOPPED_MSG);
        assert!(tmp.path().join("2026-NET-1").join("EV-9").join("EV-9_sda.001.incomplete").exists());
        // 보내는 쪽은 단순 연결 끊김이 아니라 "수신 PC에서 중지"라는 이유를 받음
        let e = f.finish().err().expect("중지 뒤 닫기는 실패해야 함");
        assert!(e.to_string().contains("중지"), "{e}");
    }

    /// 시험용 중계기: 보내는 쪽 ↔ 수신기 사이를 이어 주고, `cut`을 올리면 그때 열려 있는 연결을 모두 끊음
    /// 끊김 이후 동작을 정할 수 있는 중계기. mode: 0 그대로 전달, 1 대기 중단(이후 접속 거부), 2 접속을 받자마자 끊음
    fn relay_ctl(to: u16) -> (u16, Arc<AtomicU32>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        let mode = Arc::new(AtomicU32::new(0));
        let m = mode.clone();
        std::thread::spawn(move || {
            let mut conns: Vec<TcpStream> = Vec::new();
            loop {
                match m.load(Ordering::SeqCst) {
                    1 => {
                        for c in &conns {
                            let _ = c.shutdown(Shutdown::Both);
                        }
                        return; // 대기 소켓을 닫음 → 접속 거부
                    }
                    2 => {
                        for c in conns.drain(..) {
                            let _ = c.shutdown(Shutdown::Both);
                        }
                    }
                    _ => {}
                }
                match l.accept() {
                    Ok((a, _)) => {
                        if m.load(Ordering::SeqCst) == 2 {
                            let _ = a.shutdown(Shutdown::Both);
                            continue;
                        }
                        a.set_nonblocking(false).ok();
                        let Ok(b) = TcpStream::connect(("127.0.0.1", to)) else { continue };
                        conns.push(a.try_clone().unwrap());
                        conns.push(b.try_clone().unwrap());
                        for (mut src, mut dst) in [(a.try_clone().unwrap(), b.try_clone().unwrap()), (b, a)] {
                            std::thread::spawn(move || {
                                let mut buf = vec![0u8; 64 << 10];
                                while let Ok(n) = src.read(&mut buf) {
                                    if n == 0 || dst.write_all(&buf[..n]).is_err() {
                                        break;
                                    }
                                }
                                let _ = dst.shutdown(Shutdown::Both);
                            });
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        (port, mode)
    }

    /// 끊긴 뒤 수신 프로그램이 대기하지 않으면(접속 거부) 10분 기다리지 않고 곧 포기
    #[test]
    fn gives_up_soon_when_receiver_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let (rport, _srv) = start_receiver(tmp.path().to_path_buf());
        let (port, mode) = relay_ctl(rport);
        let (store, _) = connect_retry(port, "aaaaabbbbb", "EV-G", 64 << 20).unwrap();
        let mut f = store.create("a.dd").unwrap();
        f.write_all(&[1u8; 1 << 20]).unwrap();
        mode.store(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        let t = Instant::now();
        let mut err = None;
        for _ in 0..200 {
            if let Err(e) = f.write_all(&[2u8; 256 << 10]) {
                err = Some(e);
                break;
            }
        }
        let e = err.expect("끊긴 뒤 쓰기는 실패해야 함");
        assert!(e.to_string().contains("대기하고 있지 않습니다"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(40), "{:?}", t.elapsed());
    }

    /// 재접속을 기다리는 중에도 중단 요청은 바로 반영
    #[test]
    fn cancel_stops_reconnect_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let (rport, _srv) = start_receiver(tmp.path().to_path_buf());
        let (port, mode) = relay_ctl(rport);
        let (store, _) = connect_retry(port, "aaaaabbbbb", "EV-C", 64 << 20).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        store.set_cancel(cancel.clone());
        let mut f = store.create("a.dd").unwrap();
        f.write_all(&[1u8; 1 << 20]).unwrap();
        // 접속은 받되 바로 끊음 → 거부가 아니라 재접속을 계속 시도하는 상황
        mode.store(2, Ordering::SeqCst);
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(4));
            c2.store(true, Ordering::SeqCst);
        });
        let t = Instant::now();
        let mut err = None;
        for _ in 0..200 {
            if let Err(e) = f.write_all(&[2u8; 256 << 10]) {
                err = Some(e);
                break;
            }
        }
        let e = err.expect("중단 뒤 쓰기는 실패해야 함");
        assert!(e.to_string().contains("중단 요청"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(12), "{:?}", t.elapsed());
    }

    #[test]
    fn bidi_names_rejected() {
        assert!(crate::store::check_file_name("evidence\u{202E}fdp.exe").is_err());
        assert!(crate::case::validate_segment("사건 번호", "2026\u{2066}-1").is_err());
        assert!(crate::store::check_file_name("EV-1_HOST_20261106_sda.E01").is_ok());
    }

    fn relay(to: u16) -> (u16, Arc<AtomicU32>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let gen = Arc::new(AtomicU32::new(0));
        let g = gen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(a) = c else { return };
                let Ok(b) = TcpStream::connect(("127.0.0.1", to)) else { continue };
                let mine = g.load(Ordering::SeqCst);
                for (mut src, mut dst) in [(a.try_clone().unwrap(), b.try_clone().unwrap()), (b, a)] {
                    let g = g.clone();
                    std::thread::spawn(move || {
                        src.set_read_timeout(Some(Duration::from_millis(20))).ok();
                        let mut buf = vec![0u8; 64 << 10];
                        loop {
                            if g.load(Ordering::SeqCst) != mine {
                                let _ = src.shutdown(Shutdown::Both);
                                let _ = dst.shutdown(Shutdown::Both);
                                return;
                            }
                            match src.read(&mut buf) {
                                Ok(0) => {
                                    let _ = dst.shutdown(Shutdown::Write);
                                    return;
                                }
                                Ok(n) => {
                                    if dst.write_all(&buf[..n]).is_err() {
                                        return;
                                    }
                                }
                                Err(e) if is_timeout(&e) => {}
                                Err(_) => {
                                    let _ = dst.shutdown(Shutdown::Both);
                                    return;
                                }
                            }
                        }
                    });
                }
            }
        });
        (port, gen)
    }

    /// 읽는 위치가 정해진 지점을 지나면 연결을 끊는 원본 (전송 도중 끊김 재현)
    struct CutSource {
        data: Vec<u8>,
        cuts: Vec<u64>,
        gen: Arc<AtomicU32>,
    }
    impl crate::source::Source for CutSource {
        fn len(&self) -> u64 {
            self.data.len() as u64
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            if self.cuts.first().is_some_and(|c| off >= *c) {
                self.cuts.remove(0);
                self.gen.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
            }
            let s = off as usize;
            let n = buf.len().min(self.data.len() - s);
            buf[..n].copy_from_slice(&self.data[s..s + n]);
            Ok(n)
        }
    }

    #[test]
    fn transfer_resumes_after_connection_drops() {
        use crate::imaging::{run_image_to, ImageSpec};
        let tmp = tempfile::tempdir().unwrap();
        let (rport, srv) = start_receiver(tmp.path().to_path_buf());
        let (pport, gen) = relay(rport);
        let data = random(48 << 20, 11);
        let (store, _) = connect_retry(pport, "AAAAA-BBBBB", "EV-R", data.len() as u64).unwrap();
        let store = Arc::new(store);
        let dyn_store: Arc<dyn EvidenceStore> = store.clone();
        let mut obs = LogToStore(dyn_store.create("EV-R_collection.log").unwrap());
        let spec = ImageSpec {
            out_dir: PathBuf::new(),
            base_name: "EV-R_HOST_sda".into(),
            split_bytes: 16 << 20,
            algs: HashAlg::ALL.to_vec(),
            verify: true,
            zero_fill_errors: true,
            max_bytes: None,
            ext: None,
            format: ImageFormat::Raw,
            compression: Default::default(),
            meta: Default::default(),
        };
        // 이미지 중간(10MB, 30MB 지점)에서 두 번 연결을 끊음
        let mut src = CutSource { data: data.clone(), cuts: vec![10 << 20, 30 << 20], gen };
        let r = run_image_to(&mut src, &spec, dyn_store.clone(), &AtomicBool::new(false), &mut obs).unwrap();
        obs.0.finish().unwrap();
        assert!(r.verify.as_ref().unwrap().matched, "이어받은 뒤에도 수신 측 재검증 해시가 원본과 같아야 함");
        assert!(store.resumes() >= 1, "끊김이 이어받기로 처리되어야 함");
        assert!(!dyn_store.notes().is_empty());
        dyn_store.finish_session("완료").unwrap();
        let o = srv.join().unwrap().unwrap();
        assert!(o.resumes >= 1);
        assert!(o.incomplete.is_empty());
        let dir = tmp.path().join("2026-NET-1").join("EV-R");
        let got: Vec<u8> = r.files.iter().flat_map(|f| std::fs::read(dir.join(&f.name)).unwrap()).collect();
        assert_eq!(got.len(), data.len());
        assert!(got == data, "받은 파일 내용이 원본과 같아야 함");
    }

    #[test]
    fn stop_while_data_keeps_flowing() {
        // 보내는 쪽이 쉬지 않고 보내는 중에도 중지가 반영되어야 함 (실제 GUI 시험에서 발견)
        let tmp = tempfile::tempdir().unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let ctrl = Arc::new(ReceiveControl::default());
        let c2 = ctrl.clone();
        let out = tmp.path().to_path_buf();
        let h = std::thread::spawn(move || {
            let opts = ReceiveOptions { listen: format!("127.0.0.1:{port}"), out_base: out, code: Some("AAAAA-BBBBB".into()), resume_wait: Duration::from_secs(2) };
            let space = |_: &Path, _: u64, _: u64| Ok(("시험 매체".to_string(), "ext4".to_string(), u64::MAX, None));
            serve(&opts, &space, &mut |_| {}, &c2)
        });
        let (store, _) = connect_retry(port, "AAAAA-BBBBB", "EV-10", 1 << 40).unwrap();
        let sender = std::thread::spawn(move || {
            let mut f = store.create("EV-10_sda.dd").unwrap();
            let chunk = vec![3u8; 1 << 20];
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(20) {
                if let Err(e) = f.write_all(&chunk) {
                    return Some(e.to_string());
                }
            }
            None
        });
        std::thread::sleep(Duration::from_millis(500));
        let t = Instant::now();
        ctrl.stop();
        let e = h.join().unwrap().err().unwrap();
        assert_eq!(e.to_string(), STOPPED_MSG);
        assert!(t.elapsed() < Duration::from_secs(5), "중지가 늦음: {:?}", t.elapsed());
        let err = sender.join().unwrap().expect("보내는 쪽은 실패로 끝나야 함");
        assert!(err.contains("중지"), "{err}");
    }

    #[test]
    fn wrong_code_is_rejected_and_right_code_works() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut out = Vec::new();
            for _ in 0..2 {
                let (s, _) = l.accept().unwrap();
                let mut ch = Channel::handshake(s, "AAAAABBBBB", false, None).unwrap();
                let r = ch.recv();
                out.push(r.as_ref().map(|_| true).unwrap_or_else(|e| e.kind() != io::ErrorKind::PermissionDenied));
                if r.is_ok() {
                    ch.send(K_OK, b"pong").unwrap();
                }
            }
            out
        });
        // 틀린 코드
        let s = TcpStream::connect(addr).unwrap();
        let mut ch = Channel::handshake(s, "AAAAACCCCC", true, None).unwrap();
        ch.send(K_HELLO, b"ping").unwrap();
        // 맞는 코드
        let s = TcpStream::connect(addr).unwrap();
        let mut ch2 = Channel::handshake(s, "AAAAABBBBB", true, None).unwrap();
        ch2.send(K_HELLO, b"ping").unwrap();
        let (k, b) = ch2.recv().unwrap();
        assert_eq!((k, b.as_slice()), (K_OK, &b"pong"[..]));
        drop(ch);
        assert_eq!(t.join().unwrap(), vec![false, true]);
    }

    #[test]
    fn slow_preauth_client_is_dropped() {
        // 인증 전 단계에서 느리게 보내는 접속은 마감 시간에 끊김
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let started = Instant::now();
            let r = Channel::handshake(s, "AAAAABBBBB", false, Some(Instant::now() + Duration::from_millis(300)));
            (r.is_err(), started.elapsed())
        });
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"WSC").unwrap(); // 머리 일부만 보내고 멈춤
        let (failed, took) = t.join().unwrap();
        assert!(failed);
        assert!(took < Duration::from_secs(3));
    }
}
