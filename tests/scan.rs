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
        skip_pseudo: false,
        network: false,
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
    // 同一実体は 1 回だけ計上する (sub と link のどちらを辿るかは順序依存)。
    // 辿られなかった側は自身のメタデータサイズ (リンク長またはディレクトリ長) のみ。
    assert_eq!(root.file_count, 1);
    assert!(
        root.total_logical < 200,
        "subtree was counted twice: {}",
        root.total_logical
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
    assert!(s.ends_with("}\n"));

    let mut csv: Vec<u8> = Vec::new();
    dskr::report::write_csv(&mut csv, &out).unwrap();
    let s = String::from_utf8(csv).unwrap();
    assert!(
        s.starts_with("path,is_dir,allocated,logical,file_count,dir_count,error_count,truncated\n")
    );
    assert!(s.contains("a.bin,false,"));
}
