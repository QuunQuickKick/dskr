//! TUI からの削除処理。シンボリックリンク / ジャンクションはリンク先を
//! 辿らず、リンク自体だけを削除する。

use std::fs;
use std::path::{Path, PathBuf};

use crate::scan::node::{FileId, NodeKind};
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

/// 削除前の検証だけを行い、実際に削除する種別を返す。
///
/// 走査時の ID と現在の ID が一致しない場合、どちらかが取得できない場合は
/// エラーにする。確認ダイアログを開く前の事前チェックにも使う。
pub fn check_target(path: &Path, kind: NodeKind, expected: FileId) -> Result<NodeKind, String> {
    verify_target(path, kind, expected).map(|(kind, _)| kind)
}

/// 対象を検証し、削除に使う種別とメタデータを返す。
///
/// ファイル種別だけでなく、走査時に記録したファイル ID を再取得して照合する。
/// ID の取得・照合に失敗した場合は、意図しない削除を避けるため必ず拒否する。
fn verify_target(
    path: &Path,
    kind: NodeKind,
    expected: FileId,
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
        return Err("走査時にファイル識別情報を取得できなかったため、削除を中止しました".into());
    }
    let Some(current) = platform::file_id(&md, &fs_path, link_like) else {
        return Err("削除対象のファイル識別情報を取得できないため、削除を中止しました".into());
    };
    if current != expected {
        return Err("走査時から対象が置き換わっています (削除を中止しました)".into());
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
/// - ファイル: ファイルを削除
/// - フォルダ: 中身ごと削除
/// - リンク / ジャンクション / 特殊ファイル: リンク先を辿らず実体だけ削除
///
/// 走査後に種類やファイル ID が変わっていた場合は、意図しない削除を避けるため
/// エラーにする。ディレクトリは自身の ID だけを検証し、中身は削除実行時点の
/// 状態を削除する。
pub fn remove_target(path: &Path, kind: NodeKind, expected: FileId) -> Result<(), String> {
    let (kind, md) = verify_target(path, kind, expected)?;
    let fs_path = deletable_path(path);
    match kind {
        NodeKind::Dir => fs::remove_dir_all(&fs_path).map_err(|e| format!("削除に失敗: {e}")),
        NodeKind::File => fs::remove_file(&fs_path).map_err(|e| format!("削除に失敗: {e}")),
        NodeKind::Other | NodeKind::Blocked => remove_link_or_leaf(&fs_path, &md),
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

    fn id_of(path: &Path) -> FileId {
        let md = fs::symlink_metadata(path).unwrap();
        let link_like = md.file_type().is_symlink() || platform::is_reparse_point(&md);
        platform::file_id(&md, path, link_like).expect("ファイル ID を取得できる")
    }

    #[test]
    fn removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("victim.txt");
        fs::write(&file, b"x").unwrap();
        let id = id_of(&file);

        remove_target(&file, NodeKind::File, id).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn removes_directory_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();
        let id = id_of(&victim);

        remove_target(&victim, NodeKind::Dir, id).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn directory_delete_removes_files_added_after_scan() {
        // ディレクトリ自身の ID だけを検証し、中身は削除実行時点の状態を消す。
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(&victim).unwrap();
        let id = id_of(&victim);
        fs::write(victim.join("added-after-scan.txt"), b"x").unwrap();

        remove_target(&victim, NodeKind::Dir, id).unwrap();
        assert!(!victim.exists());
    }

    #[test]
    fn missing_target_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.txt");
        let err = remove_target(&missing, NodeKind::File, FileId::UNKNOWN).unwrap_err();
        assert!(err.contains("存在しません"), "{err}");
    }

    #[test]
    fn refuses_recursive_delete_when_kind_changed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("nested")).unwrap();
        fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();
        let id = id_of(&victim);

        let err = remove_target(&victim, NodeKind::File, id).unwrap_err();
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
        let id = id_of(&link);

        remove_target(&link, NodeKind::Other, id).unwrap();

        assert!(!link.exists());
        assert!(target.exists());
        assert!(target.join("keep.txt").exists());
    }

    #[test]
    fn refuses_directory_delete_when_target_is_file() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"x").unwrap();
        let id = id_of(&victim);

        let err = remove_target(&victim, NodeKind::Dir, id).unwrap_err();
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
        let id = id_of(&link);

        remove_target(&link, NodeKind::File, id).unwrap();

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
        let id = id_of(&victim);

        let err = remove_target(&victim, NodeKind::Blocked, id).unwrap_err();
        assert!(!err.is_empty(), "{err}");
        assert!(victim.exists());
    }

    #[test]
    fn file_replacement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"old").unwrap();
        let id = id_of(&victim);

        let replacement = dir.path().join("replacement.txt");
        fs::write(&replacement, b"new").unwrap();
        fs::remove_file(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::File, id).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert_eq!(fs::read(&victim).unwrap(), b"new");
    }

    #[test]
    fn replacement_between_check_and_delete_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"old").unwrap();
        let id = id_of(&victim);
        // 確認ダイアログを開く時点の事前チェック。
        check_target(&victim, NodeKind::File, id).unwrap();

        let replacement = dir.path().join("replacement.txt");
        fs::write(&replacement, b"new").unwrap();
        fs::remove_file(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        // 実際の削除時点では別ファイルになっている。
        let err = remove_target(&victim, NodeKind::File, id).unwrap_err();
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
        let id = id_of(&victim);

        let target = dir.path().join("target.txt");
        fs::write(&target, b"keep").unwrap();
        fs::remove_file(&victim).unwrap();
        symlink(&target, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::File, id).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
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
        let id = id_of(&victim);

        fs::remove_file(&victim).unwrap();
        fs::write(&victim, b"new").unwrap();

        let err = remove_target(&victim, NodeKind::Other, id).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert_eq!(fs::read(&victim).unwrap(), b"new");
    }

    #[test]
    fn file_to_directory_replacement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, b"old").unwrap();
        let id = id_of(&victim);

        fs::remove_file(&victim).unwrap();
        fs::create_dir(&victim).unwrap();

        let err = remove_target(&victim, NodeKind::File, id).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert!(victim.is_dir());
    }

    #[test]
    fn directory_replacement_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        fs::create_dir(&victim).unwrap();
        let id = id_of(&victim);

        let replacement = dir.path().join("replacement");
        fs::create_dir(&replacement).unwrap();
        fs::write(replacement.join("keep.txt"), b"keep").unwrap();
        fs::remove_dir(&victim).unwrap();
        fs::rename(&replacement, &victim).unwrap();

        let err = remove_target(&victim, NodeKind::Dir, id).unwrap_err();
        assert!(err.contains("置き換わっています"), "{err}");
        assert!(victim.join("keep.txt").exists());
    }

    #[test]
    fn unknown_expected_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        fs::write(&victim, b"x").unwrap();

        let err = remove_target(&victim, NodeKind::File, FileId::UNKNOWN).unwrap_err();
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
        let id = id_of(&victim);

        remove_target(&victim, NodeKind::File, id).unwrap();
        assert!(!victim.exists());
        assert_eq!(fs::read(&twin).unwrap(), b"x");
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
        let id = id_of(&link);

        remove_target(&link, NodeKind::Other, id).unwrap();
        assert!(!link.exists());
        assert!(target.join("keep.txt").exists());
    }
}
