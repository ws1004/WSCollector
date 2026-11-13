//! 읽기 원본 추상화.
//!
//! - 디스크, 볼륨은 읽기 전용(GENERIC_READ)으로만 엽니다.
//! - Windows에서는 FILE_FLAG_NO_BUFFERING으로 열어 대상 PC의 파일 캐시(=메모리)를 덮어쓰지 않습니다.
//!   이 때문에 버퍼 주소, 읽기 위치, 길이를 모두 섹터 크기 배수로 맞춥니다.
//! - 문자 없는 파티션은 물리 디스크의 일부 범위(`offset`, `len`)로 읽습니다.

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::fs::File;
use std::io;
use std::ops::{Deref, DerefMut};

/// 이미징 엔진이 읽는 원본
pub trait Source: Send {
    /// 전체 바이트 수
    fn len(&self) -> u64;
    /// 섹터 크기 (오류 재시도 단위, 정렬 단위)
    fn sector_size(&self) -> u32;
    /// `offset`에서 `buf.len()`만큼 읽습니다. 원본 끝이면 더 적게 읽을 수 있습니다.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
    /// 읽기 오류 때 다시 읽고 0으로 채우는 단위 [시작, 끝). 기본은 섹터.
    /// 메모리(LiME)는 머리말 때문에 섹터 경계와 물리 페이지가 어긋나므로 물리 페이지 단위로 알려 줌
    fn unit_at(&self, offset: u64) -> (u64, u64) {
        let s = self.sector_size() as u64;
        let a = offset / s * s;
        (a, a + s)
    }
    /// 한 구간(4MB)을 통째로 못 읽는 일이 몇 번 이어지면 중단할지 (디스크 16번 = 64MB: 장치가 사라진 것으로 봄)
    fn dead_run_limit(&self) -> u32 {
        16
    }
    /// 원본에서 읽지 않고 만들어 내는 부분인지 (LiME 머리말). 늘 성공하므로 "연속 실패","읽은 양" 집계에서 뺌
    fn is_synthetic(&self, _offset: u64) -> bool {
        false
    }
    /// 오류 위치를 사람이 읽는 말로 (메모리: 물리 주소). 없으면 원본 위치로 표시
    fn where_is(&self, _offset: u64) -> Option<String> {
        None
    }
}

/// 4096바이트 정렬 버퍼 (NO_BUFFERING 요구사항)
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
    layout: Layout,
}

unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub const ALIGN: usize = 4096;

    pub fn new(len: usize) -> Self {
        let len = len.max(Self::ALIGN);
        let layout = Layout::from_size_align(len, Self::ALIGN).expect(crate::tr!("정렬 버퍼 레이아웃", "aligned buffer layout"));
        // SAFETY: 크기가 0이 아니고 레이아웃이 유효함
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "{}", crate::tr!("정렬 버퍼 할당 실패", "aligned buffer allocation failed"));
        Self { ptr, len, layout }
    }
}

impl Deref for AlignedBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: ptr은 len 바이트 할당되어 있음
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
impl DerefMut for AlignedBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: 위와 같음, &mut self로 독점
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: 같은 레이아웃으로 할당했음
        unsafe { dealloc(self.ptr, self.layout) }
    }
}

/// 파일 핸들 기반 원본 (장치, 볼륨, 일반 파일 모두)
pub struct FileSource {
    file: File,
    base: u64,
    len: u64,
    sector: u32,
}

impl FileSource {
    /// `base`부터 `len` 바이트 범위를 원본으로 사용합니다.
    pub fn new(file: File, base: u64, len: u64, sector: u32) -> Self {
        Self { file, base, len, sector: sector.max(512) }
    }

    /// 일반 파일 (테스트, 이미지 파일 재해시 등)
    pub fn open_file(path: &std::path::Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self::new(file, 0, len, 512))
    }
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn sector_size(&self) -> u32 {
        self.sector
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        // 장치는 요청한 만큼 한 번에 읽힙니다. 짧게 읽히면 끝으로 보고 멈춤
        // (이어서 읽으면 정렬되지 않은 위치가 되어 NO_BUFFERING에서 오류 87)
        let want = buf.len().min((self.len - offset) as usize).max(0);
        let req = buf.len();
        let n = pread(&self.file, self.base + offset, &mut buf[..req])?;
        Ok(n.min(want))
    }
}

#[cfg(windows)]
fn pread(f: &File, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    f.seek_read(buf, off)
}

#[cfg(unix)]
fn pread(f: &File, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    // 시그널(EINTR)로 끊긴 읽기는 오류가 아니므로 같은 위치를 다시 읽음 (정상 섹터를 0으로 채우지 않게)
    loop {
        match f.read_at(buf, off) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            r => return r,
        }
    }
}

/// 장치 경로를 읽기 전용으로 엽니다 (NO_BUFFERING + SEQUENTIAL_SCAN).
#[cfg(windows)]
pub fn open_device_readonly(path: &str) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

/// Linux: O_DIRECT로 열어 페이지 캐시(=대상 서버 메모리)를 거치지 않습니다.
/// O_DIRECT를 거부하는 장치는 일반 읽기로 열고, 읽은 뒤 캐시를 버리도록(`DropCacheSource`) 합니다.
#[cfg(unix)]
pub fn open_device_readonly(path: &str) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "linux")]
    let direct = libc::O_DIRECT;
    #[cfg(not(target_os = "linux"))]
    let direct = 0;
    match std::fs::OpenOptions::new().read(true).custom_flags(direct).open(path) {
        Ok(f) => Ok(f),
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) || e.raw_os_error() == Some(libc::EPERM) => {
            std::fs::OpenOptions::new().read(true).open(path)
        }
        Err(e) => Err(e),
    }
}

/// 이미 읽은 범위를 페이지 캐시에서 버립니다 (O_DIRECT를 못 쓴 경우, 검증 전 재읽기 보장)
#[cfg(target_os = "linux")]
pub fn drop_cache(f: &File, off: u64, len: u64) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: 유효한 파일 디스크립터. 실패해도 동작에 영향 없음
    unsafe {
        libc::posix_fadvise(f.as_raw_fd(), off as libc::off_t, len as libc::off_t, libc::POSIX_FADV_DONTNEED);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn drop_cache(_f: &File, _off: u64, _len: u64) {}

/// 정렬 조건(캐시 없는 읽기)을 맞추지 못해 생긴 오류인지
pub fn is_alignment_error(e: &io::Error) -> bool {
    #[cfg(windows)]
    return e.raw_os_error() == Some(87); // ERROR_INVALID_PARAMETER
    #[cfg(unix)]
    return e.raw_os_error() == Some(libc::EINVAL);
}

/// 출력 파일 검증용: 캐시를 거치지 않고 다시 읽기 위해 NO_BUFFERING으로 엽니다.
#[cfg(windows)]
pub fn open_for_verify(path: &std::path::Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

#[cfg(unix)]
pub fn open_for_verify(path: &std::path::Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "linux")]
    {
        std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECT).open(path)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::fs::OpenOptions::new().read(true).custom_flags(0).open(path)
    }
}

/// 정렬된 위치에서 한 번 읽습니다 (검증용). 파일 끝이면 더 적게 읽힙니다.
pub fn read_aligned(f: &File, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    pread(f, off, buf)
}
