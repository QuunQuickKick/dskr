//! マウント境界を越えて削除しないことの実機テスト。
//!
//! CI が tmpfs / bind mount を準備したときだけ実行する。環境変数が
//! ない通常の `cargo test` では何もしない。

#![cfg(target_os = "linux")]

use std::path::PathBuf;

use dskr::delete::{remove_target, DeleteError};
use dskr::scan::node::NodeKind;
use dskr::scan::platform;

fn id_of(path: &std::path::Path) -> dskr::scan::node::FileId {
    let md = std::fs::symlink_metadata(path).expect("メタデータを取得できる");
    platform::file_id(&md, path, false).expect("ファイル ID を取得できる")
}

#[test]
fn mount_boundaries_are_not_crossed() {
    let Some(root) = std::env::var_os("DSKR_MOUNT_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let parent = root.join("parent");
    if !parent.exists() {
        return;
    }

    // マウントポイントを含むディレクトリは非空なので、再帰削除しない。
    let err = remove_target(&parent, NodeKind::Dir, id_of(&parent), false, false).unwrap_err();
    assert!(matches!(err, DeleteError::Refused(_)), "{err:?}");
    assert!(
        parent.join("with_file").join("keep.txt").exists(),
        "tmpfs 上のファイルが削除された"
    );
    assert!(
        parent.join("bind_mnt").join("keep.txt").exists(),
        "bind mount 上のファイルが削除された"
    );

    // 空のマウントポイント自体も削除しない。
    let empty = parent.join("empty_mount");
    if empty.exists() {
        let err = remove_target(&empty, NodeKind::Dir, id_of(&empty), false, false).unwrap_err();
        assert!(
            err.contains("マウントポイント") || err.contains("空ではない"),
            "{err}"
        );
        assert!(empty.exists(), "マウントポイントが削除された");
    }
}
