//! TUI からの削除処理。シンボリックリンク / ジャンクションはリンク先を
//! 辿らず、リンク自体だけを削除する。

use std::fs;
use std::path::{Path, PathBuf};

use crate::scan::node::{FileId, NodeKind};
use crate::scan::platform;

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileDispositionInfo, GetFileInformationByHandle, GetVolumeInformationByHandleW,
    SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// Windows の長いパスでも削除できるように変換する。
fn deletable_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        crate::path::to_extended(path)
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// 削除前の検証だけを行い、実際に削除する種別を返す。
///
/// 走査時の ID と現在の ID が一致しない場合、どちらかが取得できない場合は
/// エラーにする。確認ダイアログを開く前の事前チェックにも使う。
pub fn check_target(
    path: &Path,
    kind: NodeKind,
    expected: FileId,
    expected_link_like: bool,
) -> Result<NodeKind, String> {
    verify_target(path, kind, expected, expected_link_like).map(|(kind, _)| kind)
}

/// 対象を検証し、削除に使う種別とメタデータを返す。
///
/// ファイル種別だけでなく、走査時に記録したファイル ID を再取得して照合する。
/// ID の取得・照合に失敗した場合は、意図しない削除を避けるため必ず拒否する。
fn verify_target(
    path: &Path,
    kind: NodeKind,
    expected: FileId,
    expected_link_like: bool,
) -> Result<(NodeKind, fs::Metadata), String> {
    let fs_path = deletable_path(path);
    let md = fs::symlink_metadata(&fs_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("対象が存在しません: {}", path.display())
        } else {
            format!("対象を確認できません: {e}")
        }
    })?;
    let ft = md.file_type();
    let link_like = ft.is_symlink() || platform::is_reparse_point(&md);

    if !expected.is_known() {
        return Err(
            "走査時にファイル識別情報を取得できなかったため (ネットワークファイルシステム上を含む)、削除を中止しました"
                .into(),
        );
    }
    let Some(current) = platform::file_id(&md, &fs_path, link_like) else {
        return Err("削除対象のファイル識別情報を取得できないため、削除を中止しました".into());
    };
    if current != expected {
        return Err("走査時から対象が置き換わっています (削除を中止しました)".into());
    }
    if link_like != expected_link_like {
        return Err(if link_like {
            "対象がリンクに変わっています (削除を中止しました)"
        } else {
            "対象がリンクではなくなっています (削除を中止しました)"
        }
        .into());
    }

    // リンク・ジャンクションは、記録上の種別にかかわらずリンク自身だけを消す。
    // 走査後に通常ファイルからリンクへ変わった場合は、上の ID 照合で拒否される。
    let effective = if link_like {
        NodeKind::Other
    } else {
        match kind {
            NodeKind::Dir => {
                if !ft.is_dir() {
                    return Err("対象がフォルダではなくなっています".into());
                }
                NodeKind::Dir
            }
            NodeKind::File => {
                if ft.is_dir() {
                    return Err("対象がフォルダに変わっています (削除を中止しました)".into());
                }
                NodeKind::File
            }
            NodeKind::Blocked => {
                if !ft.is_dir() {
                    return Err("対象がフォルダではなくなっています".into());
                }
                NodeKind::Blocked
            }
            NodeKind::Other => {
                // 特殊ファイルが通常ファイルに置き換わった場合は削除しない。
                // ディレクトリは、リンク追従の重複として Other になった実体を
                // 空のときだけ削除できる既存動作を維持する。
                if ft.is_file() {
                    return Err("対象の種類が変わっています (削除を中止しました)".into());
                }
                NodeKind::Other
            }
        }
    };
    Ok((effective, md))
}

/// 選択された項目を削除する。`kind` は走査時に記録した種別。
///
/// - Windows の通常ファイル: 同一ハンドルで検証して disposition を設定
/// - ファイル (Windows 以外): ファイルを削除
/// - フォルダ: 中身ごと削除
/// - リンク / ジャンクション / 特殊ファイル: リンク先を辿らず実体だけ削除
///
/// 走査後に種類やファイル ID が変わっていた場合は、意図しない削除を避けるため
/// エラーにする。ディレクトリは自身の ID だけを検証し、中身は削除実行時点の
/// 状態を削除する。
pub fn remove_target(
    path: &Path,
    kind: NodeKind,
    expected: FileId,
    expected_link_like: bool,
) -> Result<(), String> {
    let (kind, md) = verify_target(path, kind, expected, expected_link_like)?;

    // Phase 2-B: Windows の通常ファイルだけをハンドルベースで削除する。
    // 対象外の種別は従来の削除経路を維持する。
    #[cfg(windows)]
    if kind == NodeKind::File && !expected_link_like {
        return remove_file_by_handle(path, expected);
    }

    let fs_path = deletable_path(path);
    match kind {
        NodeKind::Dir => fs::remove_dir_all(&fs_path).map_err(|e| format!("削除に失敗: {e}")),
        NodeKind::File => fs::remove_file(&fs_path).map_err(|e| format!("削除に失敗: {e}")),
        NodeKind::Other | NodeKind::Blocked => remove_link_or_leaf(&fs_path, &md),
    }
}

/// ハンドルベース削除の対象検証。
///
/// 実際の削除を行う前に、同じハンドルから取得した情報だけを使って
/// FileId・種別・属性・ハードリンク数・ファイルシステムを確認する。
#[cfg(windows)]
fn validate_handle_info(
    info: &BY_HANDLE_FILE_INFORMATION,
    expected: FileId,
    fs_name: &str,
) -> Result<(), String> {
    if !expected.is_known() {
        return Err(
            "走査時にファイル識別情報を取得できなかったため (ネットワークファイルシステム上を含む)、削除を中止しました"
                .into(),
        );
    }

    let volume = info.dwVolumeSerialNumber as u64;
    let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    if volume != expected.volume || index != expected.index {
        return Err("走査時から対象が置き換わっています (削除を中止しました)".into());
    }

    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        return Err("対象がフォルダに変わっています (削除を中止しました)".into());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("対象がリンク / 再解析ポイントのため、削除を中止しました".into());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_READONLY != 0 {
        return Err("読み取り専用ファイルは削除できません (属性は変更しません)".into());
    }
    if info.nNumberOfLinks != 1 {
        return Err(format!(
            "ハードリンク数が {} のため、削除を中止しました",
            info.nNumberOfLinks
        ));
    }
    if !fs_name.eq_ignore_ascii_case("NTFS") {
        return Err(format!(
            "NTFS以外のファイルシステムでは削除できません: {fs_name}"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_error(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

#[cfg(windows)]
fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn last_error() -> u32 {
    unsafe { GetLastError() }
}

/// ハンドルの閉鎖を 1 回だけ行うためのガード。
/// 早期 return でもハンドルを解放し、明示 close 後は Drop で再試行しない。
#[cfg(windows)]
struct DeleteHandle(Option<HANDLE>);

#[cfg(windows)]
impl DeleteHandle {
    fn new(handle: HANDLE) -> Self {
        Self(Some(handle))
    }

    fn raw(&self) -> HANDLE {
        self.0.expect("delete handle already closed")
    }

    fn close(&mut self) -> Result<(), u32> {
        match self.0.take() {
            Some(handle) => {
                if unsafe { CloseHandle(handle) } == 0 {
                    Err(last_error())
                } else {
                    Ok(())
                }
            }
            None => Ok(()),
        }
    }
}

#[cfg(windows)]
impl Drop for DeleteHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

#[cfg(windows)]
fn volume_fs_name(handle: HANDLE) -> Result<String, String> {
    let mut serial: u32 = 0;
    let mut max_component: u32 = 0;
    let mut fs_flags: u32 = 0;
    let mut volume_name = [0u16; 64];
    let mut fs_name = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            handle,
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
            &mut serial,
            &mut max_component,
            &mut fs_flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(format!(
            "ファイルシステムを確認できないため、削除を中止しました: {}",
            windows_error(last_error())
        ));
    }
    let len = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    Ok(String::from_utf16_lossy(&fs_name[..len]))
}

#[cfg(windows)]
fn set_delete_disposition(handle: HANDLE) -> Result<(), u32> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: 1 };
    let ok = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            &disposition as *const _ as *const c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 {
        Err(last_error())
    } else {
        Ok(())
    }
}

/// Windows ローカル NTFS の通常ファイルを、同一ハンドルで検証して削除する。
///
/// `CreateFileW` 失敗時も、検証失敗時も、path-based 削除へはフォールバックしない。
#[cfg(windows)]
fn remove_file_by_handle(path: &Path, expected: FileId) -> Result<(), String> {
    if !expected.is_known() {
        return Err(
            "走査時にファイル識別情報を取得できなかったため (ネットワークファイルシステム上を含む)、削除を中止しました"
                .into(),
        );
    }

    let fs_path = deletable_path(path);
    let wide = to_wide(&fs_path);
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(format!(
            "削除用ハンドルを取得できないため、削除を中止しました: {}",
            windows_error(last_error())
        ));
    }
    let mut handle = DeleteHandle::new(raw);

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.raw(), &mut info) } == 0 {
        return Err(format!(
            "削除対象のファイル情報を取得できないため、削除を中止しました: {}",
            windows_error(last_error())
        ));
    }
    let fs_name = volume_fs_name(handle.raw())?;
    validate_handle_info(&info, expected, &fs_name)?;

    if let Err(code) = set_delete_disposition(handle.raw()) {
        let close = handle.close();
        let mut message = format!("削除指示に失敗しました: {}", windows_error(code));
        if let Err(close_code) = close {
            message.push_str(&format!(
                " / ハンドルの解放にも失敗しました: {}",
                windows_error(close_code)
            ));
        }
        return Err(message);
    }

    match handle.close() {
        Ok(()) => Ok(()),
        Err(code) => Err(format!(
            "削除指示は成功しましたが、ハンドルの解放に失敗しました。ファイルの状態を確認できません: {}",
            windows_error(code)
        )),
    }
}

/// リンク / ジャンクション / 特殊ファイル自体を削除する。再帰はしない。
#[cfg(windows)]
fn remove_link_or_leaf(path: &Path, md: &fs::Metadata) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    if md.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        fs::remove_dir(path).map_err(|e| format!("削除に失敗: {e}"))
    } else {
        fs::remove_file(path).map_err(|e| format!("削除に失敗: {e}"))
    }
}

#[cfg(not(windows))]
fn remove_link_or_leaf(path: &Path, md: &fs::Metadata) -> Result<(), String> {
    // リンクは symlink_metadata 上で is_dir() が false になるため、
    // ここで true になるのは実体のディレクトリ (別FSなどで中身を
    // 走査していない項目) だけ。再帰せず空のときだけ削除する。
    if md.file_type().is_dir() {
        fs::remove_dir(path).map_err(|e| format!("削除に失敗: {e}"))
    } else {
        fs::remove_file(path).map_err(|e| format!("削除に失敗: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_of(path: &Path) -> (FileId, bool) {
        let md = fs::symlink_metadata(path).unwrap();
        let link_like = md.file_type().is_symlink() || platform::is_reparse_point(&md);
        let id = platform::file_id(&md, path, link_like).expect("ファイル ID を取得できる");
        (id, link_like)
    }

    #[test]
    fn removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("victim.txt");
        fs::write(&file, b"x").unwrap();
        let (id, link_like) = id_of(&file);

        remove_target(&file, NodeKind::File, id, link_like).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn removes_directory_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();
        let (id, link_like) = id_of(&victim);

        remove_target(&victim, NodeKind::Dir, id, link_like).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn directory_delete_removes_files_added_after_scan() {
        // ディレクトリ自身の ID だけを検証し、中身は削除実行時点の状態を消す。
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        let (id, link_like) = id_of(&victim);
        fs::write(victim.join("added-after-scan.txt"), b"x").unwrap();

        remove_target(&victim, NodeKind::Dir, id, link_like).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn missing_target_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.txt");
        let err = remove_target(&missing, NodeKind::File, FileId::UNKNOWN, false).unwrap_err();
        assert!(err.contains("存在しません"), "{err}");
    }

    #[test]
    fn refuses_recursive_delete_when_kind_changed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();
        let (id, link_like) = id_of(&victim);

        let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
        assert!(err.contains("フォルダ"), "{err}");
        assert!(victim.exists());
    }

    #[cfg(unix)]
    #[test]
    fn removes_symlink_without_following_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        let (id, link_like) = id_of(&link);

        remove_target(&link, NodeKind::Other, id, link_like).unwrap();

        assert!(!link.exists());
        assert!(target.exists());
        assert!(target.join("keep.txt").exists());
    }

    #[test]
    fn refuses_directory_delete_when_target_is_file() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"x").unwrap();
        let (id, link_like) = id_of(&victim);

        let err = remove_target(&victim, NodeKind::Dir, id, link_like).unwrap_err();
        assert!(err.contains("フォルダではなく"), "{err}");
        assert!(victim.exists());
    }

    #[cfg(unix)]
    #[test]
    fn removes_symlink_recorded_as_file_without_following_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"keep").unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        let (id, link_like) = id_of(&link);

        remove_target(&link, NodeKind::File, id, link_like).unwrap();

        assert!(!link.exists());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn blocked_kind_refuses_recursive_delete() {
        // Blocked (走査対象外の実ディレクトリ) は中身ごと削除しない。
        // 空でないフォルダは削除に失敗する。
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        fs::write(victim.join("a.bin"), b"x").unwrap();
        let (id, link_like) = id_of(&victim);

        let err = remove_target(&victim, NodeKind::Blocked, id, link_like).unwrap_err();
        assert!(!err.is_empty(), "{err}");
        assert!(victim.exists());
    }

    #[test]
    fn file_replacement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"old").unwrap();
        let (id, link_like) = id_of(&victim);

        let replacement = dir.path().join("replacement.txt");
        fs::write(&replacement, b"new").unwrap();
        fs::remove_file(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert_eq!(fs::read(&victim).unwrap(), b"new");
    }

    #[test]
    fn replacement_between_check_and_delete_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"old").unwrap();
        let (id, link_like) = id_of(&victim);
        // 確認ダイアログを開く時点の事前チェック。
        check_target(&victim, NodeKind::File, id, link_like).unwrap();

        let replacement = dir.path().join("replacement.txt");
        fs::write(&replacement, b"new").unwrap();
        fs::remove_file(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        // 実際の削除時点では別ファイルになっている。
        let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert_eq!(fs::read(&victim).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn file_to_symlink_replacement_is_refused() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"old").unwrap();
        let (id, link_like) = id_of(&victim);

        let target = dir.path().join("target.txt");
        fs::write(&target, b"keep").unwrap();
        fs::remove_file(&victim).unwrap();
        symlink(&target, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
        assert!(
            err.contains("リンクに変わっています") || err.contains("置き換わっています"),
            "{err}"
        );
        assert!(target.exists());
        assert!(fs::symlink_metadata(&victim)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_file_replacement_is_refused() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"keep").unwrap();
        let victim = dir.path().join("victim.txt");
        symlink(&target, &victim).unwrap();
        let (id, link_like) = id_of(&victim);

        // 置き換え用ファイルを先に作り、inode の再利用を避ける。
        let replacement = dir.path().join("replacement.txt");
        fs::write(&replacement, b"new").unwrap();
        fs::remove_file(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::Other, id, link_like).unwrap_err();
        assert!(
            err.contains("リンクではなくなっています") || err.contains("置き換わっています"),
            "{err}"
        );
        assert_eq!(fs::read(&victim).unwrap(), b"new");
    }

    #[test]
    fn file_to_directory_replacement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"old").unwrap();
        let (id, link_like) = id_of(&victim);

        fs::remove_file(&victim).unwrap();
        fs::create_dir(&victim).unwrap();

        let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
        assert!(
            err.contains("フォルダに変わっています") || err.contains("置き換わっています"),
            "{err}"
        );
        assert!(victim.is_dir());
    }

    #[test]
    fn directory_replacement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir(&victim).unwrap();
        let (id, link_like) = id_of(&victim);

        let replacement = dir.path().join("replacement");
        fs::create_dir(&replacement).unwrap();
        fs::write(replacement.join("keep.txt"), b"keep").unwrap();
        fs::remove_dir(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::Dir, id, link_like).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert!(victim.join("keep.txt").exists());
    }

    #[test]
    fn unknown_expected_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"x").unwrap();

        let err = remove_target(&victim, NodeKind::File, FileId::UNKNOWN, false).unwrap_err();
        assert!(err.contains("走査時"), "{err}");
        assert!(victim.exists());
    }

    #[test]
    fn hardlink_deletion_keeps_other_link() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"x").unwrap();
        let twin = dir.path().join("twin.txt");
        fs::hard_link(&victim, &twin).unwrap();
        let (id, link_like) = id_of(&victim);

        #[cfg(windows)]
        {
            // Phase 2-B: Windows のハンドルベース削除はハードリンクを拒否する。
            let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
            assert!(err.contains("ハードリンク"), "{err}");
            assert!(victim.exists());
            assert_eq!(fs::read(&victim).unwrap(), b"x");
            assert_eq!(fs::read(&twin).unwrap(), b"x");
        }
        #[cfg(not(windows))]
        {
            remove_target(&victim, NodeKind::File, id, link_like).unwrap();
            assert!(!victim.exists());
            assert_eq!(fs::read(&twin).unwrap(), b"x");
        }
    }

    #[cfg(windows)]
    #[test]
    fn file_symlink_replacement_is_refused() {
        use std::os::windows::fs::symlink_file;

        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"old").unwrap();
        let (id, link_like) = id_of(&victim);

        let target = dir.path().join("target.txt");
        fs::write(&target, b"keep").unwrap();
        fs::remove_file(&victim).unwrap();
        if symlink_file(&target, &victim).is_err() {
            return; // 開発者モード / 権限がない環境ではスキップ
        }

        let err = remove_target(&victim, NodeKind::File, id, link_like).unwrap_err();
        assert!(
            err.contains("リンクに変わっています") || err.contains("置き換わっています"),
            "{err}"
        );
        assert!(target.exists());
        assert!(fs::symlink_metadata(&victim)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(windows)]
    #[test]
    fn junction_delete_keeps_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        let link = dir.path().join("link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            return; // ジャンクションを作れない環境ではスキップ
        }
        let (id, link_like) = id_of(&link);

        remove_target(&link, NodeKind::Other, id, link_like).unwrap();
        assert!(!link.exists());
        assert!(target.join("keep.txt").exists());
    }

    #[cfg(windows)]
    mod windows_handle_tests {
        use super::*;
        use windows_sys::Win32::Foundation::GENERIC_READ;

        fn info_for(id: FileId) -> BY_HANDLE_FILE_INFORMATION {
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            info.dwVolumeSerialNumber = id.volume as u32;
            info.nFileIndexHigh = (id.index >> 32) as u32;
            info.nFileIndexLow = id.index as u32;
            info.nNumberOfLinks = 1;
            info
        }

        fn id() -> FileId {
            FileId {
                volume: 0x1234_5678,
                index: 0x0000_0001_2345_6789,
            }
        }

        #[test]
        fn handle_validation_accepts_ntfs_regular_file() {
            let info = info_for(id());
            validate_handle_info(&info, id(), "NTFS").unwrap();
        }

        #[test]
        fn handle_validation_rejects_id_mismatch() {
            let info = info_for(id());
            let other = FileId {
                volume: id().volume,
                index: id().index + 1,
            };
            assert!(validate_handle_info(&info, other, "NTFS").is_err());
            let other_volume = FileId {
                volume: id().volume + 1,
                index: id().index,
            };
            assert!(validate_handle_info(&info, other_volume, "NTFS").is_err());
        }

        #[test]
        fn handle_validation_rejects_unknown_expected_id() {
            let info = info_for(id());
            assert!(validate_handle_info(&info, FileId::UNKNOWN, "NTFS").is_err());
        }

        #[test]
        fn handle_validation_rejects_directory_reparse_readonly_and_links() {
            let mut info = info_for(id());
            info.dwFileAttributes = FILE_ATTRIBUTE_DIRECTORY;
            assert!(validate_handle_info(&info, id(), "NTFS")
                .unwrap_err()
                .contains("フォルダ"));

            info = info_for(id());
            info.dwFileAttributes = FILE_ATTRIBUTE_REPARSE_POINT;
            assert!(validate_handle_info(&info, id(), "NTFS")
                .unwrap_err()
                .contains("リンク"));

            info = info_for(id());
            info.dwFileAttributes = FILE_ATTRIBUTE_READONLY;
            assert!(validate_handle_info(&info, id(), "NTFS")
                .unwrap_err()
                .contains("読み取り専用"));

            info = info_for(id());
            info.nNumberOfLinks = 2;
            assert!(validate_handle_info(&info, id(), "NTFS")
                .unwrap_err()
                .contains("ハードリンク"));
        }

        #[test]
        fn handle_validation_rejects_non_ntfs() {
            let info = info_for(id());
            assert!(validate_handle_info(&info, id(), "FAT32")
                .unwrap_err()
                .contains("NTFS"));
            assert!(validate_handle_info(&info, id(), "")
                .unwrap_err()
                .contains("NTFS"));
        }

        #[test]
        fn removes_regular_file_by_handle() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("victim.txt");
            fs::write(&file, b"x").unwrap();
            let (id, link_like) = id_of(&file);

            remove_target(&file, NodeKind::File, id, link_like).unwrap();
            assert!(!file.exists());
        }

        #[test]
        fn rejects_readonly_file_without_changing_attribute() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("readonly.txt");
            fs::write(&file, b"x").unwrap();
            let (id, link_like) = id_of(&file);

            let mut permissions = fs::metadata(&file).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&file, permissions).unwrap();

            let err = remove_target(&file, NodeKind::File, id, link_like).unwrap_err();
            assert!(err.contains("読み取り専用"), "{err}");
            assert!(file.exists());
            assert_eq!(fs::read(&file).unwrap(), b"x");
            assert!(fs::metadata(&file).unwrap().permissions().readonly());

            let mut permissions = fs::metadata(&file).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&file, permissions).unwrap();
        }

        #[test]
        fn disposition_failure_is_reported_and_file_remains() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("readonly-disposition.txt");
            fs::write(&file, b"x").unwrap();
            let mut permissions = fs::metadata(&file).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&file, permissions).unwrap();

            let handle = unsafe {
                CreateFileW(
                    to_wide(&file).as_ptr(),
                    DELETE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            assert!(handle != INVALID_HANDLE_VALUE && !handle.is_null());

            let err = set_delete_disposition(handle).unwrap_err();
            assert_eq!(err, 5, "read-only disposition should be ACCESS_DENIED");
            unsafe {
                let _ = CloseHandle(handle);
            }
            assert!(file.exists());
            assert_eq!(fs::read(&file).unwrap(), b"x");

            let mut permissions = fs::metadata(&file).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&file, permissions).unwrap();
        }

        #[test]
        fn rejects_create_file_sharing_conflict_without_path_fallback() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("locked.txt");
            fs::write(&file, b"x").unwrap();
            let (id, link_like) = id_of(&file);

            // Existing handle requests read access and shares read/write but not delete.
            let lock = unsafe {
                CreateFileW(
                    to_wide(&file).as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            assert!(lock != INVALID_HANDLE_VALUE && !lock.is_null());

            let err = remove_target(&file, NodeKind::File, id, link_like).unwrap_err();
            assert!(
                err.contains("ハンドル") || err.contains("共有") || err.contains("sharing"),
                "{err}"
            );
            assert!(file.exists());
            assert_eq!(fs::read(&file).unwrap(), b"x");

            unsafe {
                let _ = CloseHandle(lock);
            }
        }
    }
}
