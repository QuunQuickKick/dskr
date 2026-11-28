//! スキャンエンジンの統合テスト。一時ディレクトリにフィクスチャを構築して検証する。

use std::fs;
use std::path::Path;
use std::sync::Arc;

use dskr::scan::engine::{Control, EngineConfig, Progress};
use dskr::scan::run;
use dskr::scan::MAX_NODES_DEFAULT;
use tempfile::tempdir;

fn cfg(root: &Path) -> EngineConfig {
    EngineConfig {
        root: root.to_path_buf(),
        follow_links: false,
        one_file_system: false,
        excludes: Vec::new(),
        include_hidden: false,
        min_size: 0,
        max_depth: None,
        max_nodes: MAX_NODES_DEFAULT,
        threads: 2,
        rate_limit: 0,
        skip_cloud_placeholders: false,
        want_allocated: true,
        want_identity: false,
        skip_pseudo: false,
        network: false,
        root_is_network: false,
        local_only: false,
    }
}

fn scan(_root: &Path, c: EngineConfig) -> dskr::scan::ScanOutcome {
    run(c, Arc::new(Progress::new()), Arc::new(Control::new()))
}

fn child(out: &dskr::scan::ScanOutcome, name: &str) -> Option<u32> {
    out.tree
        .node(out.tree.root)
        .children
        .iter()
        .copied()
        .find(|&i| out.tree.node(i).name.to_string_lossy() == name)
}

#[cfg(unix)]
fn find_path(out: &dskr::scan::ScanOutcome, names: &[&str]) -> Option<u32> {
    let mut cur = out.tree.root;
    for name in names {
        cur = out
            .tree
            .node(cur)
            .children
            .iter()
            .copied()
            .find(|&i| out.tree.node(i).name.to_string_lossy() == *name)?;
    }
    Some(cur)
}

#[test]
fn counts_and_sizes() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(dir.path().join("a.txt"), vec![0u8; 1000]).unwrap();
    fs::write(dir.path().join("b.txt"), vec![0u8; 3000]).unwrap();
    fs::write(sub.join("c.txt"), vec![0u8; 500]).unwrap();

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 3);
    assert_eq!(root.dir_count, 1);
    assert_eq!(root.total_logical, 4500);
    assert!(root.total_alloc >= root.total_logical);
    let sub_idx = child(&out, "sub").unwrap();
    assert_eq!(out.tree.node(sub_idx).total_logical, 500);
    assert_eq!(out.tree.node(sub_idx).file_count, 1);
    assert_eq!(out.error_count, 0);
}

#[cfg(unix)]
#[test]
fn hardlink_dedup() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("f1"), vec![7u8; 10]).unwrap();
    fs::hard_link(dir.path().join("f1"), dir.path().join("f2")).unwrap();
    let blocks = fs::metadata(dir.path().join("f1")).unwrap().blocks();

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 2);
    assert_eq!(root.total_logical, 20);
    assert_eq!(root.total_alloc, blocks * 512); // 同一実体は1回のみ
    assert_eq!(out.hardlink_groups.len(), 1);
    assert_eq!(
        out.hardlink_groups.values().next().unwrap().len(),
        2,
        "削除時の移し替え用に両方のノードを保持する"
    );
}

#[cfg(unix)]
#[test]
fn hardlink_representative_is_deterministic() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempdir().unwrap();
    let d1 = dir.path().join("d1");
    let d2 = dir.path().join("d2");
    fs::create_dir_all(&d1).unwrap();
    fs::create_dir_all(&d2).unwrap();
    let f1 = d1.join("f");
    let f2 = d2.join("g");
    fs::write(&f1, vec![7u8; 64]).unwrap();
    fs::hard_link(&f1, &f2).unwrap();
    let blocks = fs::metadata(&f1).unwrap().blocks() * 512;

    for _ in 0..5 {
        let out = scan(dir.path(), cfg(dir.path()));
        let n1 = find_path(&out, &["d1", "f"]).unwrap();
        let n2 = find_path(&out, &["d2", "g"]).unwrap();
        assert_eq!(
            out.tree.node(n1).own_alloc,
            blocks,
            "辞書順で先の d1/f に allocated が集約される"
        );
        assert_eq!(out.tree.node(n2).own_alloc, 0);
        assert_eq!(out.tree.node(out.tree.root).total_alloc, blocks);
    }
}

#[test]
fn symlink_not_followed_by_default() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("c.txt"), vec![0u8; 100]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&sub, dir.path().join("link")).unwrap();
    #[cfg(windows)]
    {
        // Windows ではシンボリックリンクの作成に特権が必要
        // (管理者実行または開発者モード)。権限がなければ検証をスキップする。
        use windows_sys::Win32::Foundation::ERROR_PRIVILEGE_NOT_HELD;
        match std::os::windows::fs::symlink_dir(&sub, dir.path().join("link")) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) => {
                println!(
                    "(スキップ: シンボリックリンク作成の特権がありません。管理者実行または開発者モードで検証できます)"
                );
                return;
            }
            Err(e) => panic!("symlink_dir 失敗: {e}"),
        }
    }

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1); // リンク先は計上されない
    let link = child(&out, "link").unwrap();
    assert_eq!(out.tree.node(link).kind, dskr::scan::NodeKind::Other);
}

#[cfg(unix)]
#[test]
fn follow_links_traverses() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("c.txt"), vec![0u8; 100]).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

    let out = scan(dir.path(), cfg(dir.path()));
    assert_eq!(out.tree.node(out.tree.root).file_count, 0); // 既定では辿らない

    let mut c = cfg(dir.path());
    c.follow_links = true;
    let out = scan(dir.path(), c);
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1);
    assert_eq!(root.total_logical, 100);
}

#[cfg(unix)]
#[test]
fn follow_links_dedups_already_scanned_dir() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("c.txt"), vec![0u8; 100]).unwrap();
    std::os::unix::fs::symlink(&sub, dir.path().join("link")).unwrap();

    let mut c = cfg(dir.path());
    c.follow_links = true;
    let out = scan(dir.path(), c);
    let root = out.tree.node(out.tree.root);
    // 同一実体は 1 回だけ計上する。sub と link のどちらを先に辿るかは
    // readdir の順序に依存するため、どちらの場合でも成り立つ形で検証する。
    assert_eq!(root.file_count, 1, "同じ実体のファイルは 1 回だけ計上");

    let sub_idx = child(&out, "sub").expect("sub がありません");
    let link_idx = child(&out, "link").expect("link がありません");
    let (sub_files, link_files) = (
        out.tree.node(sub_idx).file_count,
        out.tree.node(link_idx).file_count,
    );
    assert_eq!(
        sub_files + link_files,
        1,
        "サブツリーが二重計上されています (sub={sub_files}, link={link_files})"
    );

    let (counted, skipped) = if sub_files == 1 {
        (out.tree.node(sub_idx), out.tree.node(link_idx))
    } else {
        (out.tree.node(link_idx), out.tree.node(sub_idx))
    };
    assert_eq!(counted.kind, dskr::scan::NodeKind::Dir);
    assert_eq!(counted.total_logical, 100, "サブツリーの合計は 1 回分");
    assert_eq!(skipped.kind, dskr::scan::NodeKind::Other);
    assert_eq!(skipped.file_count, 0);
    assert!(skipped.children.is_empty(), "辿られない側は中身を持たない");
    assert_eq!(
        root.total_logical,
        100 + skipped.total_logical,
        "辿られない側は自身のメタデータサイズのみを加算する"
    );
}

#[test]
fn max_depth_limits() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a");
    let b = a.join("b");
    fs::create_dir_all(&b).unwrap();
    fs::write(b.join("deep.txt"), vec![0u8; 10]).unwrap();
    fs::write(a.join("shallow.txt"), vec![0u8; 10]).unwrap();

    let mut c = cfg(dir.path());
    c.max_depth = Some(1);
    let out = scan(dir.path(), c);
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 0); // a/ 内のファイル (深さ2) も対象外
    assert_eq!(root.dir_count, 1); // a (深さ1) は対象
    assert!(out.tree.node(child(&out, "a").unwrap()).truncated);

    let mut c = cfg(dir.path());
    c.max_depth = Some(2);
    let out = scan(dir.path(), c);
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1); // a/shallow.txt のみ (b/deep.txt は深さ3)
    let a_idx = child(&out, "a").unwrap();
    assert_eq!(out.tree.node(a_idx).file_count, 1);
}

#[test]
fn unicode_names_are_counted() {
    let dir = tempdir().unwrap();
    let name = "日本語 🚀 report.txt";
    fs::write(dir.path().join(name), vec![0u8; 123]).unwrap();

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1);
    assert_eq!(root.total_logical, 123);
    let idx = child(&out, name).expect("Unicode 名のノードがありません");
    assert_eq!(out.tree.node(idx).kind, dskr::scan::NodeKind::File);
}

#[cfg(target_os = "linux")]
#[test]
fn sparse_file_reports_allocated_below_logical() {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse.bin");
    let mut file = fs::File::create(&path).unwrap();
    file.set_len(8 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::Start(7 * 1024 * 1024)).unwrap();
    file.write_all(b"x").unwrap();
    drop(file);

    let md = fs::metadata(&path).unwrap();
    if md.blocks().saturating_mul(512) >= md.len() {
        println!("(スキップ: このファイルシステムはスパースを報告しません)");
        return;
    }

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1);
    assert_eq!(root.total_logical, 8 * 1024 * 1024);
    assert!(
        root.total_alloc < root.total_logical,
        "スパースの割当サイズが logical 以上です: {} >= {}",
        root.total_alloc,
        root.total_logical
    );
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_clone_is_counted() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("source.bin");
    let dst = dir.path().join("clone.bin");
    fs::write(&src, vec![0x5au8; 1024 * 1024]).unwrap();

    // APFS の clone 作成。コピー先の FS が対応していなければスキップ。
    let status = std::process::Command::new("cp")
        .args(["-c"])
        .arg(&src)
        .arg(&dst)
        .status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        println!("(スキップ: cp -c で clone を作成できません)");
        return;
    }

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 2);
    assert_eq!(root.total_logical, 2 * 1024 * 1024);
    assert!(root.total_alloc > 0);
    // clone の共有ブロック重複排除は未実装。ここでは集計が破綻しないことだけ確認する。
}

#[cfg(windows)]
#[test]
fn windows_long_path_is_scanned() {
    let base = tempdir().unwrap();
    let mut long_root = base.path().to_path_buf();
    for i in 0..16 {
        long_root.push(format!("segment_{i:02}_abcdefghij"));
    }
    assert!(long_root.as_os_str().len() > 260);

    let extended_root = dskr::path::to_extended(&long_root);
    fs::create_dir_all(&extended_root).unwrap();
    let file = long_root.join("file.txt");
    fs::write(dskr::path::to_extended(&file), b"x").unwrap();

    let out = scan(&long_root, cfg(&long_root));
    let root = out.tree.node(out.tree.root);
    assert_eq!(
        root.file_count, 1,
        "長いパス配下のファイルが集計されていません"
    );
    assert_eq!(root.total_logical, 1);
}

#[cfg(windows)]
#[test]
fn windows_junction_is_not_followed_by_default() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("inside.txt"), vec![0u8; 10]).unwrap();
    let link = dir.path().join("junction");

    // ジャンクション作成に失敗する環境 (権限・ポリシー) ではスキップする。
    let status = std::process::Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &link.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        println!("(スキップ: ジャンクションを作成できません)");
        return;
    }

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1, "リンク先を二重計上しています");
    let link_idx = child(&out, "junction").expect("junction ノードがありません");
    assert_eq!(
        out.tree.node(link_idx).kind,
        dskr::scan::NodeKind::Other,
        "ジャンクションは既定で辿らない"
    );
    assert!(out.tree.node(link_idx).children.is_empty());
}

#[test]
fn max_nodes_marks_global_truncated() {
    let dir = tempdir().unwrap();
    for i in 0..30 {
        fs::write(dir.path().join(format!("f{i}")), vec![0u8; 10]).unwrap();
    }

    let mut c = cfg(dir.path());
    c.max_nodes = 5;
    let out = scan(dir.path(), c);

    assert!(out.truncated, "max_nodes 超過が全体に反映されていません");
    assert!(
        out.tree.node(out.tree.root).truncated,
        "ルートに truncated が立っていません"
    );
}

#[test]
fn min_size_filters_files() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("small"), vec![0u8; 10]).unwrap();
    fs::write(dir.path().join("big"), vec![0u8; 2_000_000]).unwrap();

    let mut c = cfg(dir.path());
    c.min_size = 1_000_000;
    let out = scan(dir.path(), c);
    assert_eq!(out.tree.node(out.tree.root).file_count, 1);
    assert_eq!(out.tree.node(out.tree.root).total_logical, 2_000_000);
}

#[test]
fn exclude_glob() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("keep.txt"), vec![0u8; 10]).unwrap();
    fs::write(dir.path().join("skip.log"), vec![0u8; 10]).unwrap();
    let node_modules = dir.path().join("node_modules");
    fs::create_dir_all(&node_modules).unwrap();
    fs::write(node_modules.join("x.js"), vec![0u8; 10]).unwrap();

    let mut c = cfg(dir.path());
    c.excludes = vec!["*.log".into(), "node_modules".into()];
    let out = scan(dir.path(), c);
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1);
    assert_eq!(root.dir_count, 0);
}

#[test]
fn hidden_entries_are_skipped_by_default() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("visible.txt"), vec![0u8; 10]).unwrap();
    fs::write(dir.path().join(".hidden.txt"), vec![0u8; 20]).unwrap();
    let hidden_dir = dir.path().join(".hidden-dir");
    fs::create_dir_all(&hidden_dir).unwrap();
    fs::write(hidden_dir.join("secret.txt"), vec![0u8; 30]).unwrap();

    let out = scan(dir.path(), cfg(dir.path()));
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 1);
    assert_eq!(root.dir_count, 0);
    assert_eq!(root.total_logical, 10);
    assert!(child(&out, ".hidden.txt").is_none());
    assert!(child(&out, ".hidden-dir").is_none());

    // ルート自身が隠しディレクトリでも、その中は走査する。
    let out = scan(&hidden_dir, cfg(&hidden_dir));
    assert_eq!(out.tree.node(out.tree.root).file_count, 1);

    let mut c = cfg(dir.path());
    c.include_hidden = true;
    let out = scan(dir.path(), c);
    let root = out.tree.node(out.tree.root);
    assert_eq!(root.file_count, 3);
    assert_eq!(root.dir_count, 1);
    assert_eq!(root.total_logical, 60);
    assert!(child(&out, ".hidden.txt").is_some());
    assert!(child(&out, ".hidden-dir").is_some());
}

#[cfg(windows)]
#[test]
fn windows_hidden_and_system_attributes_are_skipped_by_default() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
    };

    let dir = tempdir().unwrap();
    for (name, attr) in [
        ("hidden.bin", FILE_ATTRIBUTE_HIDDEN),
        ("system.bin", FILE_ATTRIBUTE_SYSTEM),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, vec![0u8; 10]).unwrap();
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = unsafe { SetFileAttributesW(wide.as_mut_ptr(), attr) };
        assert_ne!(ok, 0, "SetFileAttributesW failed for {name}");
    }
    fs::write(dir.path().join("visible.bin"), vec![0u8; 10]).unwrap();

    let out = scan(dir.path(), cfg(dir.path()));
    assert_eq!(out.tree.node(out.tree.root).file_count, 1);
    assert!(child(&out, "hidden.bin").is_none());
    assert!(child(&out, "system.bin").is_none());

    let mut c = cfg(dir.path());
    c.include_hidden = true;
    let out = scan(dir.path(), c);
    assert_eq!(out.tree.node(out.tree.root).file_count, 3);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_hidden_flag_is_skipped_by_default() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempdir().unwrap();
    let flagged = dir.path().join("flagged.bin");
    fs::write(&flagged, vec![0u8; 10]).unwrap();
    fs::write(dir.path().join("visible.bin"), vec![0u8; 10]).unwrap();
    let c_path = CString::new(flagged.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::chflags(c_path.as_ptr(), 0x8000) }; // UF_HIDDEN
    assert_eq!(rc, 0, "chflags(UF_HIDDEN) failed");

    let out = scan(dir.path(), cfg(dir.path()));
    assert_eq!(out.tree.node(out.tree.root).file_count, 1);
    assert!(child(&out, "flagged.bin").is_none());

    let mut c = cfg(dir.path());
    c.include_hidden = true;
    let out = scan(dir.path(), c);
    assert_eq!(out.tree.node(out.tree.root).file_count, 2);
}

#[cfg(unix)]
#[test]
fn permission_errors_are_recorded() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root では権限エラーを再現できない
    }
    let dir = tempdir().unwrap();
    let denied = dir.path().join("denied");
    fs::create_dir_all(&denied).unwrap();
    fs::write(denied.join("secret"), vec![0u8; 10]).unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();

    let out = scan(dir.path(), cfg(dir.path()));
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(out.error_count >= 1);
    let d = child(&out, "denied").unwrap();
    assert_eq!(out.tree.node(d).err_count, 1);
    assert!(!out.errors.is_empty());
}

#[cfg(unix)]
#[test]
fn strict_modes_do_not_follow_links() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("c.txt"), vec![0u8; 100]).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

    for (local_only, one_fs) in [(false, true), (true, false)] {
        let mut c = cfg(dir.path());
        c.follow_links = true;
        c.local_only = local_only;
        c.one_file_system = one_fs;
        let out = scan(dir.path(), c);
        assert_eq!(
            out.tree.node(out.tree.root).file_count,
            0,
            "links must not be followed (local_only={local_only}, -x={one_fs})"
        );
    }
}

#[test]
fn rate_limit_bounds_throughput() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    for i in 0..600 {
        fs::write(sub.join(format!("f{i}")), vec![0u8; 10]).unwrap();
    }
    let mut c = cfg(dir.path());
    c.rate_limit = 1000;
    let started = std::time::Instant::now();
    let out = scan(dir.path(), c);
    let secs = started.elapsed().as_secs_f64();
    assert_eq!(out.tree.node(out.tree.root).file_count, 600);
    // 200ms 窓あたり 200 トークン。600件なら最低でも約 0.4 秒かかる。
    assert!(secs >= 0.3, "rate limit not applied: {secs}s");
    assert!(secs < 10.0, "too slow: {secs}s");
}

#[test]
fn json_and_csv_reports() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("a.bin"), vec![0u8; 12345]).unwrap();
    let out = scan(dir.path(), cfg(dir.path()));

    use dskr::report::{iso_now, ReportMeta};
    let meta = ReportMeta {
        version: "test",
        generated_at: iso_now(),
        root: dir.path().to_string_lossy().into_owned(),
        root_kind: "local",
        profile: "local".into(),
        metric: "allocated",
        allocated_collected: true,
        options: "{}".into(),
        cancelled: false,
        truncated: false,
        threads: 2,
        entries: out.entries,
        dirs: out.dirs,
        errors: out.error_count,
        elapsed_ms: out.elapsed.as_millis(),
    };
    let mut json: Vec<u8> = Vec::new();
    dskr::report::write_json(&mut json, &out, &meta).unwrap();
    let s = String::from_utf8(json).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let blocks = fs::metadata(sub.join("a.bin")).unwrap().blocks() * 512;
        assert!(s.contains(&format!("\"allocated\":{blocks}")));
    }
    #[cfg(not(unix))]
    assert!(s.contains("\"allocated\":12345"));
    assert!(s.contains("\"name\":\"a.bin\""));
    assert!(s.contains("\"schema_version\":1"));
    assert!(s.contains("\"incomplete\":false"));
    assert!(s.contains("\"errors_truncated\":false"));
    assert!(s.contains("\"blocked_folders\":0"));
    assert!(s.contains("\"allocated_collected\":true"));
    assert!(s.ends_with("}\n"));

    let mut csv: Vec<u8> = Vec::new();
    dskr::report::write_csv(&mut csv, &out, &meta).unwrap();
    let s = String::from_utf8(csv).unwrap();
    assert!(
        s.starts_with(
            "path,is_dir,allocated,logical,file_count,dir_count,error_count,truncated,cancelled,scan_truncated,scan_errors,allocated_collected,scan_blocked,schema_version\n"
        )
    );
    assert!(s.contains("a.bin,false,"));
}

#[test]
fn identity_is_collected_only_when_requested() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, b"x").unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("b.txt"), b"y").unwrap();

    let mut c = cfg(dir.path());
    c.want_identity = true;
    let out = scan(dir.path(), c);
    let a = child(&out, "a.txt").unwrap();
    let md = fs::symlink_metadata(&file).unwrap();
    let expected = dskr::scan::platform::file_id(&md, &file, false).unwrap();
    assert_eq!(out.tree.id(a), expected);

    let sub_idx = child(&out, "sub").unwrap();
    let sub_md = fs::symlink_metadata(&sub).unwrap();
    let sub_id = dskr::scan::platform::file_id(&sub_md, &sub, false).unwrap();
    assert_eq!(out.tree.id(sub_idx), sub_id);

    // 既定 (--allow-delete なし) では ID を収集しない。
    let out = scan(dir.path(), cfg(dir.path()));
    let a = child(&out, "a.txt").unwrap();
    assert_eq!(
        out.tree.id(a),
        dskr::scan::node::FileId::UNKNOWN,
        "ID を要求していないスキャンでは保持しない"
    );
}

#[test]
fn network_identity_is_suppressed_for_all_node_kinds() {
    use dskr::scan::node::FileId;

    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, b"x").unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("b.txt"), b"y").unwrap();
    #[cfg(unix)]
    let link = {
        use std::os::unix::fs::symlink;
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        link
    };

    let mut last = None;
    // ネットワークプロファイルと、Windows のネットワークルート検出の
    // どちらでも ID を収集しない。
    for (network, root_is_network) in [(true, false), (false, true)] {
        let mut c = cfg(dir.path());
        c.want_identity = true;
        c.network = network;
        c.root_is_network = root_is_network;
        let out = scan(dir.path(), c);

        assert!(out.tree.nodes.len() >= 4);
        for i in 0..out.tree.nodes.len() {
            assert_eq!(
                out.tree.id(i as u32),
                FileId::UNKNOWN,
                "ネットワーク時は全ノードで ID を保持しない (node {i})"
            );
            assert!(!out.tree.id_link_like(i as u32));
        }
        last = Some(out);
    }
    let out = last.unwrap();

    // ルート・ディレクトリ・ファイル・リンクのすべてで削除を拒否する。
    let root_kind = dskr::scan::node::NodeKind::Dir;
    let err = dskr::delete::check_target(dir.path(), root_kind, out.tree.id(0), false, false)
        .unwrap_err();
    assert!(err.contains("走査時"), "{err}");

    let file_idx = child(&out, "a.txt").unwrap();
    let file_node = out.tree.node(file_idx);
    let err = dskr::delete::remove_target(
        &file,
        file_node.kind,
        out.tree.id(file_idx),
        out.tree.id_link_like(file_idx),
        false,
    )
    .unwrap_err();
    assert!(err.contains("走査時"), "{err}");
    assert!(file.exists());

    let sub_idx = child(&out, "sub").unwrap();
    let sub_node = out.tree.node(sub_idx);
    let err = dskr::delete::remove_target(
        &sub,
        sub_node.kind,
        out.tree.id(sub_idx),
        out.tree.id_link_like(sub_idx),
        false,
    )
    .unwrap_err();
    assert!(err.contains("走査時"), "{err}");
    assert!(sub.exists());

    #[cfg(unix)]
    {
        let link_idx = child(&out, "link").unwrap();
        let link_node = out.tree.node(link_idx);
        let err = dskr::delete::remove_target(
            &link,
            link_node.kind,
            out.tree.id(link_idx),
            out.tree.id_link_like(link_idx),
            false,
        )
        .unwrap_err();
        assert!(err.contains("走査時"), "{err}");
        assert!(link.exists());
    }
}

#[cfg(unix)]
#[test]
fn followed_link_records_link_identity_not_target() {
    use std::os::unix::fs::symlink;

    let dir = tempdir().unwrap();
    let target = dir.path().join("target.txt");
    fs::write(&target, b"target").unwrap();
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();

    let mut c = cfg(dir.path());
    c.follow_links = true;
    c.want_identity = true;
    let out = scan(dir.path(), c);
    let link_idx = child(&out, "link").unwrap();

    let link_md = fs::symlink_metadata(&link).unwrap();
    let link_id = dskr::scan::platform::file_id(&link_md, &link, true).unwrap();
    let target_md = fs::metadata(&target).unwrap();
    let target_id = dskr::scan::platform::file_id(&target_md, &target, false).unwrap();

    assert_ne!(link_id, target_id);
    assert_eq!(
        out.tree.id(link_idx),
        link_id,
        "削除時の同一性はリンク自身で確認する"
    );
    assert!(
        out.tree.id_link_like(link_idx),
        "走査時のリンク種別も保持する"
    );
}
