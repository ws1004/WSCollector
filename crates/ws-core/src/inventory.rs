//! 디스크 목록에 역할(도구 매체, 저장 매체, 시스템 디스크)을 붙이고,
//! 저장 폴더가 쓸 수 있는 위치인지 검사합니다.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::model::{DiskInfo, DiskRole, VolumeInfo};
use crate::platform;

/// 디스크 목록 + 역할.
/// Linux에서는 LVM, RAID, LUKS 볼륨과 그 아래 물리 디스크가 모두 같은 역할을 받습니다.
pub fn disks_with_roles(out_dir: Option<&Path>) -> Result<Vec<DiskInfo>> {
    let mut disks = platform::list_disks()?;
    let tool = std::env::current_exe().map(|p| platform::disks_for_path(&p, &disks)).unwrap_or_default();
    let out = out_dir.map(|p| platform::disks_for_path(p, &disks)).unwrap_or_default();
    let sys = platform::system_disks(&disks);
    for d in &mut disks {
        if tool.contains(&d.number) {
            d.roles.push(DiskRole::Tool);
        }
        if out.contains(&d.number) {
            d.roles.push(DiskRole::Output);
        }
        if sys.contains(&d.number) {
            d.roles.push(DiskRole::System);
        }
    }
    Ok(disks)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputCheck {
    pub ok: bool,
    /// 사용자에게 보여줄 문장 (실패 이유 또는 매체 설명)
    pub message: String,
    pub volume: Option<VolumeInfo>,
    pub disk_model: Option<String>,
    pub disk_number: Option<u32>,
    /// 쓸 수는 있지만 알려야 하는 점 (대상 PC 시스템 디스크에 저장 등)
    pub warning: Option<String>,
}

#[cfg(windows)]
fn msg_absolute() -> &'static str {
    crate::tr!("드라이브 문자로 시작하는 전체 경로를 입력하세요. 예: E:\\Cases", "Enter a full path starting with a drive letter, e.g. E:\\Cases")
}
#[cfg(not(windows))]
fn msg_absolute() -> &'static str {
    crate::tr!("/로 시작하는 전체 경로를 입력하세요. 예: /mnt/usb/Cases", "Enter a full path starting with /, e.g. /mnt/usb/Cases")
}
#[cfg(windows)]
fn msg_no_volume() -> &'static str {
    crate::tr!(
        "이 경로의 드라이브 정보를 읽지 못했습니다. 드라이브 문자가 있는 폴더를 고르세요",
        "Could not read drive information for this path. Choose a folder on a drive with a drive letter"
    )
}
#[cfg(not(windows))]
fn msg_no_volume() -> &'static str {
    crate::tr!("이 경로가 마운트된 장치 정보를 읽지 못했습니다", "Could not read the device this path is mounted on")
}

/// 저장 폴더 검사: 전체 경로, 링크 여부, 저장 디스크.
/// 있는 폴더는 시스템 디스크여도 막지 않고 경고만 합니다 (오프라인 이미징에서는 분석 PC의 C:에 저장하는 경우가 있음).
/// 없는 폴더는 수집을 시작할 때 만듭니다 (collect.rs, net.rs create_dir_all). 단 Linux 등에서 새 폴더가
/// 시스템 디스크, 루트(/), 메모리, 장치를 모르는 위치에 생기게 되면 거부합니다 (마운트 안 된 USB 경로 방지)
pub fn check_output_dir(dir: &Path, disks: &[DiskInfo]) -> OutputCheck {
    let fail = |m: String| OutputCheck { ok: false, message: m, volume: None, disk_model: None, disk_number: None, warning: None };
    if !dir.is_absolute() {
        return fail(msg_absolute().into());
    }
    let requested = dir;
    let not_found = || fail(crate::tr!(format!("폴더가 없습니다: {}", requested.display()), format!("Folder not found: {}", requested.display())));
    let not_dir = |p: &Path| fail(crate::tr!(format!("폴더가 아니라 파일입니다: {}", p.display()), format!("This is a file, not a folder: {}", p.display())));
    // 있음 / 없음(NotFound만) / 확인 불가(권한, 끊긴 링크 등)를 구분. "없음"으로 넘겨 더 위 폴더로 검사하면 엉뚱한 볼륨을 보게 됨
    enum State {
        Dir,
        File,
        Missing,
    }
    let state = |p: &Path| -> Result<State, String> {
        match std::fs::symlink_metadata(p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::Missing),
            Err(e) => Err(crate::tr!(format!("폴더를 확인하지 못했습니다 ({}): {e}", p.display()), format!("Could not check the folder ({}): {e}", p.display()))),
            Ok(_) => match std::fs::metadata(p) {
                Ok(m) if m.is_dir() => Ok(State::Dir),
                Ok(_) => Ok(State::File),
                // 링크인데 가리키는 곳이 없음: 만들면 링크 대상에 생기거나 실패함
                Err(_) => Err(crate::tr!(format!("대상이 없는 링크입니다: {}. 실제 폴더를 고르세요", p.display()), format!("This link points nowhere: {}. Choose an actual folder", p.display()))),
            },
        }
    };
    let exists = match state(dir) {
        Ok(State::Dir) => true,
        Ok(State::File) => return not_dir(dir),
        Ok(State::Missing) => false,
        Err(m) => return fail(m),
    };
    let probe = if exists {
        dir.to_path_buf()
    } else {
        // ".." 가 섞이면 검사한 상위 폴더와 실제로 만들 위치가 달라질 수 있음
        if dir.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return fail(crate::tr!(format!("\"..\" 가 들어간 경로는 새로 만들 수 없습니다: {}. 전체 경로를 직접 입력하세요", dir.display()), format!("Cannot create a path containing \"..\": {}. Enter the full path", dir.display())));
        }
        let mut found = None;
        for a in dir.ancestors().skip(1) {
            match state(a) {
                Ok(State::Dir) => {
                    found = Some(a.to_path_buf());
                    break;
                }
                Ok(State::File) => return not_dir(a),
                Ok(State::Missing) => continue,
                Err(m) => return fail(m),
            }
        }
        // 드라이브, 마운트 자체가 없음
        let Some(a) = found else { return not_found() };
        a
    };
    let dir = probe.as_path();
    // 정션, 심볼릭 링크를 풀어서 실제로 쓰게 될 위치를 확인 (외부 매체 폴더가 시스템 디스크를 가리키는 경우)
    let Some(vol) = platform::volume_for_path(dir) else {
        return fail(msg_no_volume().into());
    };
    if vol.drive_type != "remote" {
        let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        match platform::volume_for_path(&real) {
            Some(r) if r.letter == vol.letter => {}
            Some(r) => {
                return fail(crate::tr!(
                    format!("이 폴더는 {} 위치로 연결된 링크입니다. 실제 저장할 위치를 직접 고르세요", r.letter),
                    format!("This folder is a link to {}. Choose the actual output location directly", r.letter)
                ))
            }
            None => return fail(crate::tr!("이 폴더는 다른 볼륨에 연결된 링크입니다. 실제 폴더를 고르세요", "This folder is a link to another volume. Choose the actual folder").into()),
        }
    }
    let free = crate::util::format_bytes(vol.free);
    // 새로 만들 폴더라는 안내 (사용자가 입력한 원래 경로)
    let created = if exists { String::new() } else { crate::tr!(format!(" , 새 폴더 {}는 수집을 시작할 때 만듭니다", requested.display()), format!(" , New folder {} will be created when collection starts", requested.display())) };
    if vol.drive_type == "remote" {
        // 네트워크 저장은 막지 않되 디스크 판별은 하지 않음. 아래의 "새 폴더 거부"보다 먼저 끝나지만,
        // 공유가 실제로 마운트됐을 때만 여기에 옴 (마운트 안 됐으면 상위가 로컬 디스크라 아래에서 거부)
        return OutputCheck {
            ok: true,
            message: crate::tr!(format!("네트워크 저장소 {} ({}), 여유 공간 {free}{created}", vol.letter, vol.fs), format!("Network share {} ({}), {free} free{created}", vol.letter, vol.fs)),
            volume: Some(vol),
            disk_model: None,
            disk_number: None,
            warning: None,
        };
    }
    let out_disks = platform::disks_for_path(dir, disks);
    let disk = out_disks.first().and_then(|n| disks.iter().find(|d| d.number == *n));
    let sys = platform::system_disks(disks);
    #[cfg(windows)]
    let on_system = out_disks.iter().any(|n| sys.contains(n)) || platform::system_letter().is_some_and(|l| l == vol.letter);
    #[cfg(not(windows))]
    let on_system = out_disks.iter().any(|n| sys.contains(n));
    // Linux 등: 없는 폴더를 시스템 디스크, 루트(/), 메모리, 장치를 모르는 위치에 새로 만들지 않음.
    // 매체가 마운트되지 않은 채 "/mnt/usb/Cases"를 입력하면 대상 시스템 디스크에 폴더가 생겨 증거가 써짐.
    // 루트가 overlay, ZFS 등이라 시스템 디스크를 못 알아보는 환경도 있으므로 "/" 와 장치 불명도 거부.
    // Windows는 매체가 없으면 드라이브 문자 자체가 없어 이 사고가 없고, C:\새폴더는 오프라인 이미징, 수신 PC에서 정상 사용이라 경고만.
    #[cfg(not(windows))]
    if !exists {
        let place = if on_system || vol.letter == "/" {
            Some(crate::tr!("이 PC의 시스템 디스크", "this PC's system disk"))
        } else if vol.drive_type == "ram" {
            Some(crate::tr!("메모리(tmpfs)", "memory (tmpfs)"))
        } else if !platform::output_devices_known(dir) {
            Some(crate::tr!("어느 디스크인지 확인할 수 없는 위치", "a location whose disk cannot be determined"))
        } else {
            None
        };
        if let Some(place) = place {
            return fail(crate::tr!(
                format!("폴더가 없습니다: {}. 새로 만들면 {place}({})에 생깁니다. 저장 매체가 마운트됐는지 확인하세요 (이 위치가 맞으면 폴더를 직접 만든 뒤 다시 고르세요)", requested.display(), vol.letter),
                format!("Folder not found: {}. Creating it would put it on {place} ({}). Check that the output media is mounted (if this location is intended, create the folder yourself and choose it again)", requested.display(), vol.letter)
            ));
        }
    }
    let warning = if on_system {
        Some(crate::tr!(
            "이 PC의 시스템 디스크입니다. 오프라인 이미징이면 괜찮지만, 라이브 수집이면 대상 디스크에 증거 파일을 쓰게 되어 삭제된 영역이 덮어써질 수 있습니다",
            "This is this PC's system disk. Fine for offline imaging, but in live collection evidence files would be written to the target disk and may overwrite deleted data"
        )
        .to_string())
    } else if vol.drive_type == "ram" {
        Some(crate::tr!("메모리(tmpfs) 위치입니다. 재부팅하거나 전원이 꺼지면 증거 파일이 사라집니다", "This is a memory (tmpfs) location. Evidence files will be lost on reboot or power loss").to_string())
    } else if !platform::output_devices_known(dir) {
        // 장치를 모르면 "겹치지 않음"으로 조용히 넘어가지 않고 알림 (ZFS인데 udev 없음, 컨테이너 overlay 등)
        Some(crate::tr!(
            format!(
                "저장 위치({}, {})가 어느 디스크에 있는지 확인하지 못했습니다. 이미징할 디스크가 저장 위치와 같은 디스크가 아닌지 직접 확인하세요",
                vol.letter, vol.fs
            ),
            format!(
                "Could not determine which disk holds the output location ({}, {}). Make sure the disk you image is not the one holding the output",
                vol.letter, vol.fs
            )
        ))
    } else {
        None
    };
    let message = match disk {
        Some(d) => crate::tr!(format!("{}, {}, 여유 공간 {free}{created}", d.model, d.kind_label()), format!("{}, {}, {free} free{created}", d.model, d.kind_label())),
        None => crate::tr!(format!("{} ({}), 여유 공간 {free}{created}", vol.letter, vol.fs), format!("{} ({}), {free} free{created}", vol.letter, vol.fs)),
    };
    OutputCheck { ok: true, message, disk_model: disk.map(|d| d.model.clone()), disk_number: disk.map(|d| d.number), volume: Some(vol), warning }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 시험 폴더 (Windows %TEMP%, Linux /tmp - 보통 시스템 디스크, 루트, tmpfs)
    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("wsc-inv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn missing_folder_rules() {
        let disks = disks_with_roles(None).unwrap_or_default();
        let base = tmp("missing");
        let target = base.join("a").join("Cases");
        // 볼륨 정보를 못 읽는 환경(컨테이너 등)은 판정할 수 없으므로 건너뜀
        if platform::volume_for_path(&std::env::temp_dir()).is_none() {
            return;
        }
        let oc = check_output_dir(&target, &disks);
        if cfg!(windows) {
            // Windows: 시스템 디스크(C:)여도 새 폴더 허용 + 안내 (오프라인 이미징, 수신 PC)
            assert!(oc.ok, "{}", oc.message);
            assert!(oc.message.contains(&target.display().to_string()), "새 폴더 안내가 있어야 함: {}", oc.message);
        } else {
            // Linux: /tmp 는 루트, 시스템 디스크, tmpfs 중 하나 → 새로 만들지 않음
            assert!(!oc.ok, "{}", oc.message);
            assert!(oc.message.contains(crate::tr!("마운트됐는지", "mounted")), "{}", oc.message);
        }
        assert!(!base.exists(), "검사만으로 폴더를 만들면 안 됨");
    }

    #[test]
    fn existing_folder_file_and_parent_dir() {
        let disks = disks_with_roles(None).unwrap_or_default();
        let base = tmp("exist");
        std::fs::create_dir_all(&base).unwrap();
        if platform::volume_for_path(&base).is_some() {
            let oc = check_output_dir(&base, &disks);
            assert!(oc.ok, "{}", oc.message);
            assert!(!oc.message.contains(&base.display().to_string()), "있는 폴더에 새 폴더 안내가 붙으면 안 됨: {}", oc.message);
        }
        // 파일, 파일 아래 경로는 거부
        let file = base.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        let oc = check_output_dir(&file, &disks);
        assert!(!oc.ok && oc.message.contains("f.txt"), "{}", oc.message);
        let oc = check_output_dir(&file.join("Cases"), &disks);
        assert!(!oc.ok && oc.message.contains("f.txt"), "{}", oc.message);
        // 없는 경로에 ".." 가 있으면 거부
        let oc = check_output_dir(&base.join("x").join("..").join("y"), &disks);
        assert!(!oc.ok && oc.message.contains(".."), "{}", oc.message);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_link_rejected() {
        let disks = disks_with_roles(None).unwrap_or_default();
        let base = tmp("link");
        std::fs::create_dir_all(&base).unwrap();
        let link = base.join("out");
        std::os::unix::fs::symlink(base.join("nowhere"), &link).unwrap();
        let oc = check_output_dir(&link, &disks);
        assert!(!oc.ok, "{}", oc.message);
        assert!(!base.join("nowhere").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
