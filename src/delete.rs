//! TUI からの削除処理。シンボリックリンク / ジャンクションはリンク先を
//! 辿らず、リンク自体だけを削除する。

use std::fs;
use std::path::{Path, PathBuf};

use crate::scan::node::NodeKind;
use crate::scan::platform;

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

/// 選択された項目を削除する。`kind` は走査時に記録した種別。
///
/// - ファイル: ファイルを削除
/// - フォルダ: 中身ごと削除
/// - リンク / ジャンクション / 特殊ファイル: リンク先を辿らず実体だけ削除
///
/// 走査後に種類が変わっていた場合は、意図しない再帰削除を避けるため
/// エラーにする。
pub fn remove_target(path: &Path, kind: NodeKind) -> Result<(), String> {
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

    // Blocked (-x 境界などで走査しなかった実ディレクトリ) も再帰削除はしない。
    // 実ディレクトリなら空のときだけ削除できる。
    if link_like || matches!(kind, NodeKind::Other | NodeKind::Blocked) {
        remove_link_or_leaf(&fs_path, &md)?;
        return Ok(());
    }

    match kind {
        NodeKind::Dir => {
            if !ft.is_dir() {
                return Err("対象がフォルダではなくなっています".into());
            }
            fs::remove_dir_all(&fs_path).map_err(|e| format!("削除に失敗: {e}"))
        }
        NodeKind::File => {
            if ft.is_dir() {
                return Err("対象がフォルダに変わっています (削除を中止しました)".into());
            }
            fs::remove_file(&fs_path).map_err(|e| format!("削除に失敗: {e}"))
        }
        NodeKind::Other | NodeKind::Blocked => unreachable!("Other / Blocked は上で処理済み"),
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

    #[test]
    fn removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("victim.txt");
        fs::write(&file, b"x").unwrap();

        remove_target(&file, NodeKind::File).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn removes_directory_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();

        remove_target(&victim, NodeKind::Dir).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn missing_target_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.txt");
        let err = remove_target(&missing, NodeKind::File).unwrap_err();
        assert!(err.contains("存在しません"), "{err}");
    }

    #[test]
    fn refuses_recursive_delete_when_kind_changed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();

        let err = remove_target(&victim, NodeKind::File).unwrap_err();
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

        remove_target(&link, NodeKind::Other).unwrap();

        assert!(!link.exists());
        assert!(target.exists());
        assert!(target.join("keep.txt").exists());
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

        let err = remove_target(&victim, NodeKind::Blocked).unwrap_err();
        assert!(!err.is_empty(), "{err}");
        assert!(victim.exists());
    }
}
