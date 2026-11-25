//! プラットフォーム固有処理: 割当サイズ、ハードリンク判定、ボリューム識別、
//! プロセス優先度・I/O 優先度の適用。

use std::ffi::OsStr;
use std::fs::Metadata;
use std::path::Path;

#[cfg(windows)]
use crate::cli::IoPriority;
#[cfg(unix)]
use crate::cli::IoPriority;

/// ファイルのディスク上の占有量 (割当サイズ)。
/// Unix は st_blocks×512。Windows は GetCompressedFileSizeW (want=true のとき)。
#[cfg(unix)]
pub fn file_allocated(md: &Metadata, _path: &Path, _want: bool) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.blocks().saturating_mul(512)
}

#[cfg(windows)]
pub fn file_allocated(md: &Metadata, path: &Path, want: bool) -> u64 {
    if want {
        get_compressed_size(path).unwrap_or_else(|| md.len())
    } else {
        md.len()
    }
}

/// ハードリンク重複排除用のキー (同一実体の判定)。取得できない場合は None。
#[cfg(unix)]
pub fn hardlink_key(md: &Metadata, _path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    if md.is_file() {
        Some((md.dev(), md.ino()))
    } else {
        None
    }
}

/// Windows ではファイルindexが std から取得できないため、
/// ハンドルを開いて GetFileInformationByHandle で取得する。
#[cfg(windows)]
pub fn hardlink_key(_md: &Metadata, path: &Path) -> Option<(u64, u64)> {
    file_info_by_handle(path)
}

#[cfg(windows)]
fn file_info_by_handle(path: &Path) -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    let wide = to_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if ok == 0 || info.dwVolumeSerialNumber == 0 {
        return None;
    }
    let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    if index == 0 {
        return None;
    }
    Some((info.dwVolumeSerialNumber as u64, index))
}

/// ディレクトリが属するボリュームの識別子 (--one-file-system 用)。
#[cfg(unix)]
pub fn volume_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|md| md.dev())
}

#[cfg(windows)]
pub fn volume_of(path: &Path) -> Option<u64> {
    volume_serial_by_handle(path)
}

/// ディレクトリ実体の識別キー (リンク追従時のループ検出用)。
#[cfg(unix)]
pub fn dir_key(md: &Metadata, _path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((md.dev(), md.ino()))
}

#[cfg(windows)]
pub fn dir_key(_md: &Metadata, path: &Path) -> Option<(u64, u64)> {
    file_info_by_handle(path)
}

/// 再解析ポイント (ジャンクション / マウントポイント / クラウドフォルダ) か。
#[cfg(windows)]
pub fn is_reparse_point(md: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub fn is_reparse_point(_md: &Metadata) -> bool {
    false
}

/// ネットワークファイルシステムか (--local-only の強制モードで使用)。
#[cfg(target_os = "linux")]
pub fn is_network_fs(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return false;
    }
    let magic = st.f_type as u64;
    matches!(
        magic,
        0x6969          // NFS
            | 0xFF53_4D42   // CIFS
            | 0xFE53_4D42   // SMB2
            | 0x517B        // SMB
            | 0x0102_1997   // 9p (WSL の /mnt/c など)
            | 0x6573_5546   // FUSE (sshfs, rclone 等)
            | 0x5346_414F   // AFS
            | 0x7375_7245   // CODA
            | 0x564C        // NCP
            | 0x00C3_6400   // CEPH
            | 0x4750_4653   // GPFS
            | 0x6165_676C   // LUSTRE
            | 0x0187 // AUTOFS (アクセス時に自動マウントされるため除外)
    )
}

#[cfg(target_os = "macos")]
pub fn is_network_fs(path: &Path) -> bool {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return false;
    }
    let name = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_string_lossy();
    matches!(
        name.as_ref(),
        "nfs" | "smbfs" | "afpfs" | "webdav" | "ftp" | "cddafs" | "autofs"
    )
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn is_network_fs(_path: &Path) -> bool {
    false
}

/// Windows ではネットワーク判定をボリューム一致で行うため、ここでは false。
#[cfg(windows)]
pub fn is_network_fs(_path: &Path) -> bool {
    false
}

/// リンク先が明らかにルートと別のボリューム/サーバーを指すか (Windows)。
///
/// `read_link` は再解析ポイント自体を読むだけでリンク先へはアクセスしないため、
/// 判定時点で外部へ通信することはない。
#[cfg(windows)]
pub fn link_leaves_volume(link: &Path, root_drive: Option<u8>) -> bool {
    let target = match std::fs::read_link(link) {
        Ok(t) => t,
        // 判定できない再解析ポイントは辿らない (安全側)
        Err(_) => return true,
    };
    let raw = target.to_string_lossy().into_owned();
    // "\??\" (NT オブジェクト名前空間) と "\\?\" (拡張長) の両方を除去
    let s = raw
        .strip_prefix("\\??\\")
        .or_else(|| raw.strip_prefix("\\\\?\\"))
        .unwrap_or(&raw);
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let d = bytes[0].to_ascii_uppercase();
        return match root_drive {
            Some(r) => d != r.to_ascii_uppercase(),
            None => true,
        };
    }
    // UNC / デバイス UNC はネットワーク先
    if s.starts_with("UNC\\")
        || s.starts_with("unc\\")
        || s.starts_with("\\\\")
        || s.starts_with("//")
    {
        return true;
    }
    // ボリューム GUID 指定は同一ボリュームか判定できないため辿らない
    s.starts_with("Volume{") || s.starts_with("volume{")
}

#[cfg(not(windows))]
pub fn link_leaves_volume(_link: &Path, _root_drive: Option<u8>) -> bool {
    false
}

/// 名前ベースの隠し判定。Unix のドットファイルに加え、Windows 上でも
/// `.env` のようなドット始まりを隠し項目として扱う。
pub fn is_hidden_name(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// OS の属性による隠し / システムファイル判定。
///
/// Windows は HIDDEN / SYSTEM 属性、macOS は UF_HIDDEN フラグを見る。
/// それ以外の OS には対応する属性がないため false。
#[cfg(windows)]
pub fn has_hidden_or_system_attribute(md: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    md.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(target_os = "macos")]
pub fn has_hidden_or_system_attribute(md: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const UF_HIDDEN: u32 = 0x8000;
    md.st_flags() & UF_HIDDEN != 0
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn has_hidden_or_system_attribute(_md: &Metadata) -> bool {
    false
}

/// クラウド プレースホルダ (OneDrive 等) の属性を持つか。
#[cfg(windows)]
pub fn is_cloud_placeholder(md: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x4000_00;
    let a = md.file_attributes();
    a & (FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(not(windows))]
pub fn is_cloud_placeholder(_md: &Metadata) -> bool {
    false
}

/// mtime をエポック秒へ (失敗時 0)。
pub fn mtime_secs(md: &Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 優先度の適用 (ベストエフォート)。戻り値はユーザー向け警告文。
// ---------------------------------------------------------------------------

pub fn apply_priority(nice: Option<i32>, io: Option<IoPriority>) -> Vec<String> {
    let mut warnings = Vec::new();
    #[cfg(unix)]
    {
        if let Some(n) = nice {
            if let Err(w) = unix_set_nice(n) {
                warnings.push(w);
            }
        }
        if let Some(p) = io {
            if let Ok(Some(w)) = unix_set_io_priority(p) {
                warnings.push(w)
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = nice; // Windows では nice 値は使用しない (プリセット側でも渡さない)
        if io.is_some() {
            if let Some(w) = windows_lower_priority() {
                warnings.push(w);
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (nice, io);
    }
    warnings
}

#[cfg(unix)]
fn unix_set_nice(n: i32) -> Result<(), String> {
    use std::os::raw::c_int;
    let n = n.clamp(0, 19) as c_int;
    let ret = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, n) };
    if ret != 0 {
        return Err("nice 値を適用できませんでした (権限不足)".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn unix_set_io_priority(p: IoPriority) -> Result<Option<String>, String> {
    // ioprio_set(2): which=IOPRIO_WHO_PROCESS(1), who=0 (自分)
    // class: BEST_EFFORT=2 (level 7), IDLE=3 (level 0)
    let (class, level) = match p {
        IoPriority::Idle => (3u64, 0u64),
        IoPriority::Low => (2u64, 7u64),
        IoPriority::Normal => return Ok(None),
    };
    let data = (class << 13) | level;
    let which: libc::c_ulong = 1;
    let who: libc::c_ulong = 0;
    let ret = unsafe { ioprio_set_syscall(which, who, data) };
    if ret != 0 {
        return Ok(Some("I/O 優先度 (ioprio) を適用できませんでした".into()));
    }
    Ok(None)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn ioprio_set_syscall(which: libc::c_ulong, who: libc::c_ulong, data: u64) -> libc::c_long {
    libc::syscall(libc::SYS_ioprio_set, which, who, data as libc::c_ulong)
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
unsafe fn ioprio_set_syscall(which: libc::c_ulong, who: libc::c_ulong, data: u64) -> libc::c_long {
    libc::syscall(libc::SYS_ioprio_set, which, who, data as libc::c_ulong)
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
unsafe fn ioprio_set_syscall(
    _which: libc::c_ulong,
    _who: libc::c_ulong,
    _data: u64,
) -> libc::c_long {
    -1 // 未対応アーキテクチャでは何もしない
}

#[cfg(target_os = "macos")]
fn unix_set_io_priority(_p: IoPriority) -> Result<Option<String>, String> {
    // macOS の setiopolicy_np は定数値が公開ヘッダに依存し、検証できないまま
    // 推測値で呼ぶと意図しないポリシーを設定する恐れがあるため使用しない。
    // nice (setpriority) のみを適用する。
    Ok(Some(
        "macOS では I/O 優先度の制御に未対応です (nice のみ適用)".into(),
    ))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unix_set_io_priority(_p: IoPriority) -> Result<Option<String>, String> {
    Ok(Some(
        "このプラットフォームでは I/O 優先度の制御に対応していません".into(),
    ))
}

#[cfg(windows)]
fn windows_lower_priority() -> Option<String> {
    use windows_sys::Win32::System::Threading::PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, ProcessPowerThrottling, SetPriorityClass,
        SetProcessInformation, SetThreadPriority, BELOW_NORMAL_PRIORITY_CLASS,
        PROCESS_POWER_THROTTLING_STATE, THREAD_PRIORITY_BELOW_NORMAL,
    };
    unsafe {
        let p = GetCurrentProcess();
        if SetPriorityClass(p, BELOW_NORMAL_PRIORITY_CLASS) == 0 {
            return Some("プロセス優先度を下げられませんでした".into());
        }
        let t = GetCurrentThread();
        let _ = SetThreadPriority(t, THREAD_PRIORITY_BELOW_NORMAL);
        // EcoQoS (Process Power Throttling) を要求。未対応環境では無視される。
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: 1,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        let _ = SetProcessInformation(
            p,
            ProcessPowerThrottling,
            &state as *const PROCESS_POWER_THROTTLING_STATE as *const core::ffi::c_void,
            core::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
    None
}

// ---------------------------------------------------------------------------
// Windows API ヘルパ
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn to_wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// GetCompressedFileSizeW による占有サイズ (スパース/圧縮を反映)。
#[cfg(windows)]
pub fn get_compressed_size(path: &Path) -> Option<u64> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_SUCCESS};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};
    let wide = to_wide(path);
    let mut high: u32 = 0;
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == INVALID_FILE_SIZE {
        // INVALID_FILE_SIZE は正当な値のこともあるため GetLastError を確認
        if unsafe { GetLastError() } != ERROR_SUCCESS {
            return None;
        }
    }
    Some(((high as u64) << 32) | low as u64)
}

/// ボリュームシリアル番号を取得 (ネットワークドライブ判定 / -x 用)。
#[cfg(windows)]
fn volume_serial_by_handle(path: &Path) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetVolumeInformationByHandleW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let wide = to_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0, // 属性問い合わせのみ
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut() as *mut core::ffi::c_void,
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let mut serial: u32 = 0;
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if ok != 0 {
        Some(serial as u64)
    } else {
        None
    }
}

/// ドライブがネットワークドライブ (DRIVE_REMOTE) かどうか。
#[cfg(windows)]
pub fn drive_is_remote(drive: u8) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOTE: u32 = 4;
    let root = format!("{}:\\", drive as char);
    let mut wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let t = unsafe { GetDriveTypeW(wide.as_mut_ptr()) };
    t == DRIVE_REMOTE
}

#[cfg(not(windows))]
pub fn drive_is_remote(_drive: u8) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_names_are_dot_prefixed() {
        assert!(is_hidden_name(OsStr::new(".env")));
        assert!(is_hidden_name(OsStr::new(".git")));
        assert!(!is_hidden_name(OsStr::new("env")));
        assert!(!is_hidden_name(OsStr::new("a.env")));
    }
}
