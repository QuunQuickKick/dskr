//! プラットフォーム固有処理: 割当サイズ、ハードリンク判定、ボリューム識別、
//! プロセス優先度・I/O 優先度の適用。

use std::ffi::OsStr;
use std::fs::Metadata;
use std::path::Path;

use super::node::FileId;

#[cfg(windows)]
use crate::cli::IoPriority;
#[cfg(unix)]
use crate::cli::IoPriority;

/// ファイル自体のサイズ情報。割当サイズとハードリンク重複排除用のキー。
///
/// Windows では 1 回のハンドルオープンで両方を取得する (F3 最適化)。
pub struct FileSizeInfo {
    /// ディスク上の占有量 (割当サイズ)。
    pub alloc: u64,
    /// `alloc` が実際の割当サイズとして取得できたか。
    /// false のときは見かけのサイズなどで代替している。
    pub alloc_exact: bool,
    /// 同一実体の判定キー。ハードリンク数が 2 以上のファイルのみ設定される。
    /// 取得できない、または重複排除が不要な場合は None。
    pub hardlink: Option<(u64, u64)>,
    /// 削除時の同一性検証用の ID。要求されない、または取得できない場合は None。
    pub id: Option<FileId>,
}

/// ファイルの割当サイズとハードリンクキーを取得する。
///
/// - Unix: メタデータのみ (追加のシステムコールなし)。`st_blocks × 512`。
/// - Windows: 割当サイズとハードリンクキーを 1 回の `CreateFileW` で取得し、
///   失敗時・不審な値時は従来の `GetCompressedFileSizeW` にフォールバックする。
///   `DSKR_NO_MERGED_HANDLE` を設定すると常に従来経路を使う。
pub fn file_size_info(
    md: &Metadata,
    path: &Path,
    want_alloc: bool,
    want_hardlink: bool,
    want_identity: bool,
) -> FileSizeInfo {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        let _ = want_alloc;
        FileSizeInfo {
            alloc: md.blocks().saturating_mul(512),
            alloc_exact: true,
            // リンク数 1 のファイルは重複排除の対象外。キーを保持しないことで
            // スキャン後のハードリンクグループも実際の重複だけに限定する。
            hardlink: if want_hardlink && md.is_file() && md.nlink() > 1 {
                Some((md.dev(), md.ino()))
            } else {
                None
            },
            id: if want_identity {
                file_id(md, path, false)
            } else {
                None
            },
        }
    }
    #[cfg(windows)]
    {
        if !want_alloc && !want_hardlink && !want_identity {
            return FileSizeInfo {
                alloc: md.len(),
                alloc_exact: false,
                hardlink: None,
                id: None,
            };
        }
        if merged_handle_disabled() {
            return fallback_size_info(md, path, want_alloc, want_hardlink, want_identity);
        }
        match file_size_info_by_handle(md, path, want_alloc, want_hardlink, want_identity) {
            Some(info) => info,
            None => fallback_size_info(md, path, want_alloc, want_hardlink, want_identity),
        }
    }
}

/// 従来経路 (GetCompressedFileSizeW + ハンドルオープン)。
#[cfg(windows)]
fn fallback_size_info(
    md: &Metadata,
    path: &Path,
    want_alloc: bool,
    want_hardlink: bool,
    want_identity: bool,
) -> FileSizeInfo {
    let (alloc, alloc_exact) = if want_alloc {
        match get_compressed_size(path) {
            Some(a) => (a, true),
            None => (md.len(), false),
        }
    } else {
        (md.len(), false)
    };
    FileSizeInfo {
        alloc,
        alloc_exact,
        hardlink: if want_hardlink {
            hardlink_info_by_handle(path)
        } else {
            None
        },
        id: if want_identity {
            file_id(md, path, false)
        } else {
            None
        },
    }
}

/// 統合ハンドル経路を無効化するフィールドテスト用の逃し道。
#[cfg(windows)]
fn merged_handle_disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var_os("DSKR_NO_MERGED_HANDLE").is_some())
}

#[cfg(windows)]
fn file_size_info_by_handle(
    md: &Metadata,
    path: &Path,
    want_alloc: bool,
    want_hardlink: bool,
    want_identity: bool,
) -> Option<FileSizeInfo> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileCompressionInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_COMPRESSION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    // 属性問い合わせのみ。FILE_READ_ATTRIBUTES はデータ読み出しを含まないため
    // クラウド プレースホルダを水和させない。
    let wide = to_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
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

    let mut hardlink = None;
    let mut id = None;
    if want_hardlink || want_identity {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        if ok == 0 || info.dwVolumeSerialNumber == 0 {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        if index == 0 {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }
        if want_identity {
            id = Some(FileId {
                volume: info.dwVolumeSerialNumber as u64,
                index,
            });
        }
        // リンク数 1 のファイルは重複排除の対象外。
        if want_hardlink && info.nNumberOfLinks > 1 {
            hardlink = Some((info.dwVolumeSerialNumber as u64, index));
        }
    }

    let mut alloc = md.len();
    let mut alloc_exact = false;
    if want_alloc {
        let mut comp: FILE_COMPRESSION_INFO = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileCompressionInfo,
                &mut comp as *mut FILE_COMPRESSION_INFO as *mut core::ffi::c_void,
                std::mem::size_of::<FILE_COMPRESSION_INFO>() as u32,
            )
        };
        unsafe {
            let _ = CloseHandle(handle);
        }
        if ok == 0 {
            return None;
        }
        let a = comp.CompressedFileSize.max(0) as u64;
        // 不審な値 (空ファイルなのに 0 以外、見かけサイズ + 4MiB を超える値) は
        // 従来経路で取り直す。
        let logical = md.len();
        if (logical == 0 && a != 0) || a > logical.saturating_add(4 << 20) {
            return None;
        }
        alloc = a;
        alloc_exact = true;
    } else {
        unsafe {
            let _ = CloseHandle(handle);
        }
    }

    Some(FileSizeInfo {
        alloc,
        alloc_exact,
        hardlink,
        id,
    })
}

#[cfg(windows)]
fn file_info_by_handle(path: &Path) -> Option<(u64, u64)> {
    handle_file_info(path, false)
}

/// ハードリンクの重複排除用キー。リンク数 1 のファイルは None。
#[cfg(windows)]
fn hardlink_info_by_handle(path: &Path) -> Option<(u64, u64)> {
    handle_file_info(path, true)
}

#[cfg(windows)]
fn handle_file_info(path: &Path, hardlink_only: bool) -> Option<(u64, u64)> {
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
    if hardlink_only && info.nNumberOfLinks <= 1 {
        return None;
    }
    let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    if index == 0 {
        return None;
    }
    Some((info.dwVolumeSerialNumber as u64, index))
}

/// 削除時の同一性検証に使うファイル ID を取得する。
///
/// Unix は `symlink_metadata` 相当の `md` から `(device, inode)` を返す。
/// Windows は `path` を開いて `(volume serial, file index)` を返す。
/// `link_like` が true のときはリンク先を辿らず、リンク自身の ID を返す。
#[cfg(unix)]
pub fn file_id(md: &Metadata, _path: &Path, _link_like: bool) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    let (volume, index) = (md.dev(), md.ino());
    if volume == 0 || index == 0 {
        return None;
    }
    Some(FileId { volume, index })
}

#[cfg(windows)]
pub fn file_id(_md: &Metadata, path: &Path, link_like: bool) -> Option<FileId> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if link_like {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    let wide = to_wide(path);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
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
    Some(FileId {
        volume: info.dwVolumeSerialNumber as u64,
        index,
    })
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

/// ファイルシステムの安全性分類。
///
/// `Unknown` は「ローカルと確認できない」ことを表し、削除用 ID の収集を
/// 禁止する。`--local-only` の既存挙動とは別に扱う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsClass {
    Local,
    Network,
    Unknown,
}

/// 1 回の `statfs` から得る分類結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsInfo {
    /// 削除用 ID を収集してよいかの分類。
    pub class: FsClass,
    /// 既存 `--local-only` の `is_network_fs()` と同じ判定。
    pub local_only_network: bool,
}

impl FsInfo {
    const fn new(class: FsClass, local_only_network: bool) -> Self {
        FsInfo {
            class,
            local_only_network,
        }
    }
}

/// メタデータからファイルシステムの識別子 (Unix の st_dev) を取り出す。
/// 追加の stat は行わない。Windows では None。
pub fn metadata_volume(md: &Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(md.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        None
    }
}

/// パスのファイルシステムを分類する。
///
/// `statfs` 失敗、未知の種別、その他の Unix は `Unknown` を返す。
/// これは削除用 ID を収集しない安全側の判定であり、`--local-only` の
/// ネットワーク判定とは独立している。
#[cfg(target_os = "linux")]
pub fn classify_fs(path: &Path) -> FsInfo {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return FsInfo::new(FsClass::Unknown, false),
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return FsInfo::new(FsClass::Unknown, false);
    }
    linux_fs_info(st.f_type as u64)
}

#[cfg(target_os = "linux")]
fn linux_fs_info(magic: u64) -> FsInfo {
    // 既存の --local-only 判定を維持するためのネットワーク一覧。
    let local_only_network = matches!(
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
            | 0x6165_676C   // LUSTRE (値の由来が不明瞭なため ID 分類では Unknown)
            | 0x0187 // AUTOFS
    );

    let class = match magic {
        // 明確なネットワークファイルシステム。
        0x6969 | 0xFF53_4D42 | 0xFE53_4D42 | 0x517B | 0x0102_1997 | 0x5346_414F
        | 0x7375_7245 | 0x564C | 0x00C3_6400 | 0x4750_4653 => FsClass::Network,
        // FUSE / autofs / 由来が不明瞭なものは、種別だけではローカルと
        // 断定できないため Unknown。
        0x6573_5546 | 0x0187 | 0x6165_676C => FsClass::Unknown,
        // 明確なローカルファイルシステムのホワイトリスト。
        0xEF53          // ext2/3/4
        | 0x58465342    // XFS
        | 0x9123683E    // Btrfs
        | 0x2FC12FC1    // ZFS
        | 0xF2F52010    // F2FS
        | 0x2011BAB0    // exFAT
        | 0x4D44        // FAT / vfat
        | 0x5346544E    // NTFS
        | 0x9660        // ISO9660
        | 0x15013346    // UDF
        | 0x73717368    // SquashFS
        | 0x3434        // NILFS2
        | 0x3153464A    // JFS
        | 0x52654973    // ReiserFS
        | 0x7461636F    // OCFS2
        | 0x01161970    // GFS2
        | 0xCA451A4E    // bcachefs
        | 0x01021994    // tmpfs
        | 0x858458F6    // ramfs / rootfs
        | 0x794C7630    // overlayfs
        | 0xF15F        // eCryptfs
        | 0x137F        // Minix v1
        | 0x2468        // Minix v2
        | 0x4D5A        // Minix v3
        => FsClass::Local,
        _ => FsClass::Unknown,
    };
    FsInfo::new(class, local_only_network)
}

#[cfg(target_os = "macos")]
pub fn classify_fs(path: &Path) -> FsInfo {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    let c = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return FsInfo::new(FsClass::Unknown, false),
    };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return FsInfo::new(FsClass::Unknown, false);
    }
    let name = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_string_lossy();
    macos_fs_info(st.f_flags, name.as_ref())
}

#[cfg(target_os = "macos")]
fn macos_fs_info(flags: u32, fstypename: &str) -> FsInfo {
    // 既存の --local-only 判定を維持する。
    let local_only_network = matches!(
        fstypename,
        "nfs" | "smbfs" | "afpfs" | "webdav" | "ftp" | "cddafs" | "autofs"
    );

    // 明確なネットワーク名を最優先する。
    let class = if matches!(
        fstypename,
        "nfs" | "smbfs" | "afpfs" | "webdav" | "ftp" | "cddafs"
    ) {
        FsClass::Network
    } else if fstypename == "autofs" {
        // autofs はトリガであり、マウント先の種別を保証しない。
        FsClass::Unknown
    } else if flags & (libc::MNT_LOCAL as u32) != 0 {
        FsClass::Local
    } else {
        FsClass::Unknown
    };
    FsInfo::new(class, local_only_network)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn classify_fs(_path: &Path) -> FsInfo {
    FsInfo::new(FsClass::Unknown, false)
}

/// Windows ではネットワーク判定をボリューム一致で行うため、ここでは Unknown。
#[cfg(windows)]
pub fn classify_fs(_path: &Path) -> FsInfo {
    FsInfo::new(FsClass::Unknown, false)
}

/// ネットワークファイルシステムか (既存 `--local-only` 用)。
pub fn is_network_fs(path: &Path) -> bool {
    classify_fs(path).local_only_network
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

/// mtime をエポック秒へ (取得できない場合は 0。JSON では null になる)。
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

    #[cfg(unix)]
    #[test]
    fn unix_allocation_is_exact_and_hardlink_key_requires_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a");
        std::fs::write(&file, b"x").unwrap();
        let md = std::fs::metadata(&file).unwrap();
        let info = file_size_info(&md, &file, true, true, true);
        assert!(info.alloc_exact);
        assert_eq!(info.hardlink, None, "リンク数1ではキーを作らない");
        assert!(info.id.is_some(), "ID を要求したら取得する");

        let twin = dir.path().join("b");
        std::fs::hard_link(&file, &twin).unwrap();
        let md = std::fs::metadata(&file).unwrap();
        let info = file_size_info(&md, &file, true, true, true);
        assert!(info.alloc_exact);
        assert!(info.hardlink.is_some(), "ハードリンクではキーを作る");
        assert_eq!(info.id, file_id(&md, &file, false));
    }

    #[test]
    fn file_id_is_stable_and_hardlinks_share_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a");
        std::fs::write(&file, b"x").unwrap();
        let twin = dir.path().join("b");
        std::fs::hard_link(&file, &twin).unwrap();

        let md = std::fs::symlink_metadata(&file).unwrap();
        let id = file_id(&md, &file, false).expect("ID を取得できる");
        assert!(id.is_known());
        assert_eq!(Some(id), file_id(&md, &file, false));

        let twin_md = std::fs::symlink_metadata(&twin).unwrap();
        assert_eq!(Some(id), file_id(&twin_md, &twin, false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_fs_classes_are_tri_state_and_preserve_local_only() {
        let network = [
            0x6969,
            0xFF53_4D42,
            0xFE53_4D42,
            0x517B,
            0x0102_1997,
            0x5346_414F,
            0x7375_7245,
            0x564C,
            0x00C3_6400,
            0x4750_4653,
        ];
        for magic in network {
            let info = linux_fs_info(magic);
            assert_eq!(info.class, FsClass::Network, "{magic:#x}");
            assert!(info.local_only_network, "{magic:#x}");
        }

        for magic in [0x6573_5546, 0x0187, 0x6165_676C] {
            let info = linux_fs_info(magic);
            assert_eq!(info.class, FsClass::Unknown, "{magic:#x}");
            assert!(info.local_only_network, "{magic:#x}");
        }

        for magic in [
            0xEF53, 0x58465342, 0x9123683E, 0x2FC12FC1, 0xF2F52010, 0x2011BAB0, 0x4D44, 0x5346544E,
            0x9660, 0x15013346, 0x73717368, 0x3434, 0x3153464A, 0x52654973, 0x7461636F, 0x01161970,
            0xCA451A4E, 0x01021994, 0x858458F6, 0x794C7630, 0xF15F,
        ] {
            let info = linux_fs_info(magic);
            assert_eq!(info.class, FsClass::Local, "{magic:#x}");
            assert!(!info.local_only_network, "{magic:#x}");
        }

        let unknown = linux_fs_info(0xDEAD_BEEF);
        assert_eq!(unknown.class, FsClass::Unknown);
        assert!(!unknown.local_only_network);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_fs_classes_use_network_names_then_mnt_local() {
        let local = libc::MNT_LOCAL as u32;
        for name in ["apfs", "hfs", "exfat", "msdos"] {
            let info = macos_fs_info(local, name);
            assert_eq!(info.class, FsClass::Local, "{name}");
            assert!(!info.local_only_network, "{name}");
        }

        for name in ["nfs", "smbfs", "afpfs", "webdav", "ftp", "cddafs"] {
            let info = macos_fs_info(0, name);
            assert_eq!(info.class, FsClass::Network, "{name}");
            assert!(info.local_only_network, "{name}");
        }

        let autofs = macos_fs_info(local, "autofs");
        assert_eq!(autofs.class, FsClass::Unknown);
        assert!(autofs.local_only_network);

        let unknown = macos_fs_info(0, "unknownfs");
        assert_eq!(unknown.class, FsClass::Unknown);
        assert!(!unknown.local_only_network);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_volume_matches_metadata_device() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let md = std::fs::metadata(dir.path()).unwrap();
        assert_eq!(metadata_volume(&md), Some(md.dev()));
    }
}

/// Windows 実機検証 (プラン Phase 3 / F3 のチェックリスト)。
///
/// Windows の通常 PowerShell で次を実行し、出力をそのまま共有してください。
///
/// ```powershell
/// cargo test --release -- --nocapture
///
/// # OneDrive プレースホルダ / UNC / 実 4GiB も確認する場合 (任意):
/// $env:DSKR_WINTEST_DIR = "C:\Users\<user>\OneDrive"   # または "\\localhost\C$\Users"
/// $env:DSKR_WINTEST_BIG = "1"                          # 実 4GiB ファイルを作成 (数秒)
/// cargo test --release -- --nocapture
/// ```
///
/// 検証内容:
/// 1. 各種ファイル (通常 / NTFS圧縮 / スパース / 空 / 1バイト / 4GiB超) で、
///    統合ハンドル経路の割当サイズが従来の GetCompressedFileSizeW と一致するか
/// 2. ハードリンク2件のキー一致 (重複排除の前提)
/// 3. ハンドル数の増加がないこと (開放漏れ)
/// 4. 処理速度の比較 (旧: GetCompressedFileSizeW+別ハンドル / 新: 統合ハンドル)
/// 5. DSKR_WINTEST_DIR で実ディレクトリ (OneDrive・UNC 含む) を全件比較し、
///    プレースホルダが水和 (ダウンロード) されていないことも確認
#[cfg(all(test, windows))]
mod win_real_env_tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom, Write};
    use std::os::windows::fs::MetadataExt;
    use std::path::PathBuf;
    use std::time::Instant;

    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    /// 上記 3 属性のマスク (クラウド プレースホルダ)。
    const PLACEHOLDER_MASK: u32 = FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

    /// 擬似乱数 (圧縮されにくいデータ)。外部クレートを使わないため簡易 LCG。
    fn fill_pseudo_random(buf: &mut [u8]) {
        let mut x: u64 = 0x243F_6A88_85A3_08D3;
        for b in buf.iter_mut() {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (x >> 33) as u8;
        }
    }

    fn run_simple(cmd: &str, args: &[&str]) -> bool {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// NTFS 圧縮を設定する。失敗 (FAT32 など) は false。
    fn try_compress(path: &Path) -> bool {
        run_simple("compact", &["/c", &path.display().to_string()])
    }

    /// スパース化して中央 1MiB を穴 (未割当) にする。失敗は false。
    fn try_make_sparse(path: &Path) -> bool {
        let p = path.display().to_string();
        if !run_simple("fsutil", &["sparse", "setflag", &p]) {
            return false;
        }
        run_simple("fsutil", &["sparse", "setrange", &p, "1048576", "1048576"])
    }

    struct Case {
        name: &'static str,
        path: PathBuf,
    }

    /// 検証用フィクスチャを作る。作れない種別は出力して省略する。
    /// 戻り値は (ケース一覧, ハードリンク2件のパス対)。
    fn build_fixtures(dir: &Path) -> (Vec<Case>, Option<(PathBuf, PathBuf)>) {
        let mut cases = Vec::new();
        let mut hardlinks = None;

        // 通常ファイル (1MiB, 圧縮されないデータ)
        let p = dir.join("normal.bin");
        let mut buf = vec![0u8; 1 << 20];
        fill_pseudo_random(&mut buf);
        if File::create(&p).and_then(|mut f| f.write_all(&buf)).is_ok() {
            cases.push(Case {
                name: "通常 (1MiB)",
                path: p,
            });
        } else {
            println!("  スキップ: 通常ファイルを作成できません");
        }

        // NTFS 圧縮ファイル (2MiB のゼロ → 圧縮で激減するはず)
        let p = dir.join("compressed.bin");
        if File::create(&p)
            .and_then(|mut f| f.write_all(&vec![0u8; 2 << 20]))
            .is_ok()
        {
            if try_compress(&p) {
                cases.push(Case {
                    name: "NTFS圧縮 (2MiBゼロ)",
                    path: p,
                });
            } else {
                println!("  スキップ: compact が失敗 (ボリュームが圧縮非対応の可能性)");
                let _ = fs::remove_file(&p);
            }
        } else {
            println!("  スキップ: 圧縮用ファイルを作成できません");
        }

        // スパースファイル (2MiB 書き込み後、中央 1MiB を穴にする)
        let p = dir.join("sparse.bin");
        let mut buf = vec![0u8; 2 << 20];
        fill_pseudo_random(&mut buf);
        if File::create(&p).and_then(|mut f| f.write_all(&buf)).is_ok() {
            if try_make_sparse(&p) {
                cases.push(Case {
                    name: "スパース (2MiB, 穴1MiB)",
                    path: p,
                });
            } else {
                println!("  スキップ: fsutil sparse が失敗 (権限の可能性)");
                let _ = fs::remove_file(&p);
            }
        } else {
            println!("  スキップ: スパース用ファイルを作成できません");
        }

        // 空ファイル / 1 バイト
        let p = dir.join("empty.txt");
        if File::create(&p).is_ok() {
            cases.push(Case {
                name: "空 (0バイト)",
                path: p,
            });
        }
        let p = dir.join("tiny.txt");
        if File::create(&p).and_then(|mut f| f.write_all(b"x")).is_ok() {
            cases.push(Case {
                name: "1バイト",
                path: p,
            });
        }

        // 4GiB 超 (シークで論理サイズだけ作る。NTFS では即時・実ディスク消費なし)
        let p = dir.join("large_gap.bin");
        let big = File::create(&p).and_then(|mut f| {
            f.seek(SeekFrom::Start(0x1_0000_0000))?;
            f.write_all(b"x")
        });
        if big.is_ok() {
            cases.push(Case {
                name: "4GiB超 (gap)",
                path: p,
            });
        } else {
            println!("  スキップ: 4GiB超ファイルを作成できません (FAT32 の可能性)");
            let _ = fs::remove_file(&p);
        }

        // ハードリンク 2 件
        let base = dir.join("hardlink_base.bin");
        let twin = dir.join("hardlink_twin.bin");
        if File::create(&base)
            .and_then(|mut f| f.write_all(&[7u8; 4096]))
            .is_ok()
            && fs::hard_link(&base, &twin).is_ok()
        {
            cases.push(Case {
                name: "ハードリンク1",
                path: base.clone(),
            });
            cases.push(Case {
                name: "ハードリンク2",
                path: twin.clone(),
            });
            hardlinks = Some((base, twin));
        } else {
            println!("  スキップ: ハードリンクを作成できません");
            let _ = fs::remove_file(&base);
            let _ = fs::remove_file(&twin);
        }

        (cases, hardlinks)
    }

    /// 1 ファイルの新旧比較。返り値は (名前, 論理, 旧alloc, 新alloc, 旧key, 新key, 直接経路成功)。
    fn compare_one(
        case: &Case,
    ) -> (
        String,
        u64,
        u64,
        u64,
        Option<(u64, u64)>,
        Option<(u64, u64)>,
        bool,
    ) {
        let md = fs::symlink_metadata(&case.path).unwrap();
        let old_alloc = get_compressed_size(&case.path);
        let old_key = hardlink_info_by_handle(&case.path);
        let new = file_size_info(&md, &case.path, true, true, false);
        let direct_ok = file_size_info_by_handle(&md, &case.path, true, true, false).is_some();
        (
            case.name.to_string(),
            md.len(),
            old_alloc.unwrap_or(u64::MAX),
            new.alloc,
            old_key,
            new.hardlink,
            direct_ok,
        )
    }

    #[test]
    fn merged_handle_matches_legacy_on_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let (cases, hardlinks) = build_fixtures(dir.path());
        assert!(
            cases.len() >= 4,
            "基本フィクスチャ (通常/空/1バイト) が作成できません"
        );

        println!("=== Windows 実機検証: 統合ハンドル vs 従来経路 (GetCompressedFileSizeW) ===");
        if cfg!(debug_assertions) {
            println!("(注意: デバッグビルドのため速度比は参考値です)");
        }
        println!(
            "{:<24} {:>14} {:>14} {:>14}  結果",
            "種別", "見かけ", "旧alloc", "新alloc"
        );

        let mut ok = true;
        for case in &cases {
            let (name, logical, old_alloc, new_alloc, old_key, new_key, direct_ok) =
                compare_one(case);
            let alloc_ok = old_alloc == new_alloc;
            let key_ok = old_key == new_key;
            ok &= alloc_ok && key_ok && direct_ok;
            println!(
                "{:<24} {:>14} {:>14} {:>14}  {}",
                name,
                logical,
                old_alloc,
                new_alloc,
                if alloc_ok && key_ok && direct_ok {
                    "PASS"
                } else {
                    "FAIL"
                }
            );
            if !direct_ok {
                println!("    → 統合ハンドル経路が使えずフォールバックしています (要報告)");
            }
            if !alloc_ok {
                println!("    → 割当サイズ不一致: 旧={old_alloc} 新={new_alloc} (要報告)");
            }
            if !key_ok {
                println!("    → ハードリンクキー不一致: 旧={old_key:?} 新={new_key:?} (要報告)");
            }
            assert!(alloc_ok, "{name}: 割当サイズが従来と不一致");
            assert!(key_ok, "{name}: ハードリンクキーが従来と不一致");
            assert!(direct_ok, "{name}: 統合ハンドル経路が使用できませんでした");
        }

        // ハードリンクの 2 パスが同一キーを持つこと (重複排除の前提)
        if let Some((base, twin)) = &hardlinks {
            let k1 = hardlink_info_by_handle(base);
            let k2 = hardlink_info_by_handle(twin);
            assert_eq!(k1, k2, "ハードリンク2件のキーが一致しません");
            assert!(k1.is_some(), "ハードリンクキーが取得できません");
            println!("ハードリンク重複排除用キー: 一致 (キー={k1:?})");
        }

        // 処理速度比較
        let rounds = 2000u32;
        let mut old_elapsed = std::time::Duration::ZERO;
        let mut new_elapsed = std::time::Duration::ZERO;
        let names: Vec<PathBuf> = cases.iter().map(|c| c.path.clone()).collect();
        let mds: Vec<_> = names
            .iter()
            .map(|p| fs::symlink_metadata(p).unwrap())
            .collect();
        for _ in 0..rounds {
            for (p, md) in names.iter().zip(&mds) {
                let t0 = Instant::now();
                let _ = get_compressed_size(p);
                let _ = file_info_by_handle(p);
                old_elapsed += t0.elapsed();
                let t1 = Instant::now();
                let _ = file_size_info(md, p, true, true, false);
                new_elapsed += t1.elapsed();
            }
        }
        println!(
            "速度 ({rounds}回×{}ファイル): 旧 {:?} / 新 {:?} / {:.2}倍",
            names.len(),
            old_elapsed,
            new_elapsed,
            old_elapsed.as_secs_f64() / new_elapsed.as_secs_f64().max(1e-9)
        );

        // ハンドル開放漏れチェック
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
        let count = |mut n: u32| -> u32 {
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) };
            n
        };
        let before = count(0);
        for _ in 0..3000 {
            for (p, md) in names.iter().zip(&mds) {
                let _ = file_size_info(md, p, true, true, false);
            }
        }
        let after = count(0);
        println!("ハンドル数: {before} → {after}");
        assert!(
            after <= before + 16,
            "ハンドル開放漏れの疑い: {before} → {after}"
        );
        if ok {
            println!("=== 全ケース PASS ===");
        }
    }

    /// 実 4GiB のファイル (検証に時間がかかるため DSKR_WINTEST_BIG=1 のときのみ)。
    #[test]
    fn merged_handle_matches_legacy_on_real_4gib() {
        if std::env::var_os("DSKR_WINTEST_BIG").is_none() {
            println!("(スキップ: DSKR_WINTEST_BIG=1 を設定すると実 4GiB ファイルで検証します)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("real_4gib.bin");
        let chunk = vec![0xA5u8; 4 << 20];
        let mut f = File::create(&path).unwrap();
        for _ in 0..1024 {
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"tail").unwrap();
        drop(f);
        let md = fs::symlink_metadata(&path).unwrap();
        let old_alloc = get_compressed_size(&path).unwrap();
        let new = file_size_info(&md, &path, true, true, false);
        println!(
            "実4GiBファイル: 見かけ={} 旧alloc={old_alloc} 新alloc={} → {}",
            md.len(),
            new.alloc,
            if old_alloc == new.alloc {
                "PASS"
            } else {
                "FAIL"
            }
        );
        assert_eq!(new.alloc, old_alloc, "実4GiBで割当サイズが不一致");
    }

    /// DSKR_WINTEST_DIR で指定した実ディレクトリ (OneDrive・UNC 可) を
    /// 最大 10,000 ファイル比較する。
    #[test]
    fn live_directory_matches_legacy() {
        let Some(root) = std::env::var_os("DSKR_WINTEST_DIR").map(PathBuf::from) else {
            println!("(スキップ: DSKR_WINTEST_DIR を設定すると実ディレクトリを全件比較します)");
            println!(
                "例: $env:DSKR_WINTEST_DIR = \"C:\\Windows\\System32\" (OneDrive や UNC も可)"
            );
            return;
        };

        println!("=== 実ディレクトリ検証: {} ===", root.display());
        let mut stack = vec![root.clone()];
        let mut files: Vec<PathBuf> = Vec::new();
        let cap = 10_000usize;
        while let Some(d) = stack.pop() {
            if files.len() >= cap {
                break;
            }
            let rd = match fs::read_dir(&d) {
                Ok(rd) => rd,
                Err(_) => continue, // 権限なし等は無視
            };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                let p = e.path();
                if ft.is_dir() {
                    stack.push(p);
                } else if files.len() < cap {
                    files.push(p);
                }
            }
        }
        println!("比較ファイル数: {} (上限 {cap})", files.len());

        // プレースホルダ属性を比較前に記録し、検証後に変化していないことを確認する
        let mut placeholders: Vec<(PathBuf, u32)> = Vec::new();
        for p in &files {
            if let Ok(md) = fs::symlink_metadata(p) {
                let a = md.file_attributes() & PLACEHOLDER_MASK;
                if a != 0 {
                    placeholders.push((p.clone(), a));
                }
            }
        }
        println!(
            "クラウド プレースホルダ: {} 件 (検証後に水和していないことを確認)",
            placeholders.len()
        );

        let mut mismatches = 0usize;
        let mut alloc_ok = 0usize;
        let mut old_elapsed = std::time::Duration::ZERO;
        let mut new_elapsed = std::time::Duration::ZERO;
        for p in &files {
            let Ok(md) = fs::symlink_metadata(p) else {
                continue;
            };
            let t0 = Instant::now();
            let old_alloc = get_compressed_size(p).unwrap_or(md.len());
            old_elapsed += t0.elapsed();
            let t1 = Instant::now();
            let new = file_size_info(&md, p, true, true, false);
            new_elapsed += t1.elapsed();
            if old_alloc == new.alloc {
                alloc_ok += 1;
            } else if mismatches < 10 {
                println!(
                    "  不一致: {} 見かけ={} 旧={old_alloc} 新={}",
                    p.display(),
                    md.len(),
                    new.alloc
                );
                mismatches += 1;
            } else {
                mismatches += 1;
            }
        }
        println!(
            "割当サイズ一致: {alloc_ok} / {} 件、不一致: {mismatches} 件",
            files.len()
        );
        println!("速度: 旧 {old_elapsed:?} / 新 {new_elapsed:?}");

        // プレースホルダの水和チェック (属性が RECALL から通常に変わったら水和済み)
        let mut hydrated = 0usize;
        for (p, before) in &placeholders {
            if let Ok(md) = fs::symlink_metadata(p) {
                let after = md.file_attributes() & PLACEHOLDER_MASK;
                if after != *before {
                    hydrated += 1;
                }
            }
        }
        println!("水和されたプレースホルダ: {hydrated} 件");
        assert_eq!(mismatches, 0, "割当サイズの不一致があります (詳細は上記)");
        assert_eq!(hydrated, 0, "検証中にプレースホルダが水和されました");
    }
}
