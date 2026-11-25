use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

fn dskr() -> &'static str {
    env!("CARGO_BIN_EXE_dskr")
}

fn run(args: &[&str]) -> Output {
    Command::new(dskr())
        .args(args)
        .output()
        .expect("failed to run dskr")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn make_tree(root: &Path) {
    let big = root.join("big");
    let small = root.join("small");
    fs::create_dir_all(&big).unwrap();
    fs::create_dir_all(&small).unwrap();
    fs::write(big.join("large.bin"), vec![0u8; 3 * 1024 * 1024]).unwrap();
    fs::write(small.join("small.bin"), vec![0u8; 64 * 1024]).unwrap();
    fs::write(root.join("root.txt"), vec![0u8; 2000]).unwrap();
}

#[test]
fn default_headless_is_report_and_largest_first() {
    let dir = tempdir().unwrap();
    make_tree(dir.path());
    let root = dir.path().to_string_lossy().into_owned();
    let out = run(&[&root, "--no-tui", "--color", "never"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains("dskr レポート"), "{text}");
    let big = text.find("big").expect("big directory in report");
    let small = text.find("small").expect("small directory in report");
    assert!(big < small, "largest directory should come first:\n{text}");
}

#[test]
fn json_flag_alone_is_headless_and_writes_file() {
    let dir = tempdir().unwrap();
    make_tree(dir.path());
    let root = dir.path().to_string_lossy().into_owned();
    let json = dir.path().join("out.json");
    let out = run(&[&root, "--json", json.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data = fs::read_to_string(&json).unwrap();
    assert!(data.starts_with('{'));
    assert!(data.contains("\"tree\""));
    // JSON だけを指定した場合は標準出力にレポートを出さない
    assert!(stdout(&out).trim().is_empty());
}

#[test]
fn tree_and_flat_formats() {
    let dir = tempdir().unwrap();
    make_tree(dir.path());
    let root = dir.path().to_string_lossy().into_owned();

    let out = run(&[
        &root, "--format", "tree", "--ascii", "--depth", "1", "--color", "never",
    ]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("|--") || text.contains("`--"));
    assert!(
        !text.contains("large.bin"),
        "depth 1 should not show nested files"
    );

    let out = run(&[&root, "--format", "flat", "--top", "3", "--color", "never"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 3);
    assert!(text.contains("large.bin") || text.contains("big"));
}

#[test]
fn hidden_entries_are_not_output_by_default() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("visible.txt"), vec![0u8; 10]).unwrap();
    fs::write(dir.path().join(".secret.txt"), vec![0u8; 10]).unwrap();
    let root = dir.path().to_string_lossy().into_owned();

    let out = run(&[&root, "--format", "flat", "--color", "never"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains("visible.txt"), "{text}");
    assert!(!text.contains(".secret.txt"), "{text}");

    let out = run(&[&root, "--format", "flat", "--hidden", "--color", "never"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(text.contains(".secret.txt"), "{text}");

    // JSON にも名前が漏れないこと。
    let json = dir.path().join("out.json");
    let out = run(&[&root, "--json", json.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let data = fs::read_to_string(&json).unwrap();
    assert!(!data.contains(".secret.txt"), "{data}");
}

#[test]
fn markdown_and_html_outputs_are_written() {
    let dir = tempdir().unwrap();
    make_tree(dir.path());
    let root = dir.path().to_string_lossy().into_owned();
    let md = dir.path().join("report.md");
    let html = dir.path().join("report.html");

    let out = run(&[
        &root,
        "--format",
        "markdown",
        "--output",
        md.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(&md).unwrap();
    assert!(text.contains("# ディスク使用量レポート"));
    assert!(!text.contains("http://"));

    let out = run(&[
        &root,
        "--format",
        "html",
        "--output",
        html.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(&html).unwrap();
    assert!(text.contains("Content-Security-Policy"));
    assert!(!text.contains("http://"));
    assert!(!text.contains("https://"));
    assert!(!text.contains("<script src"));
}

#[test]
fn help_is_os_specific() {
    let out = run(&["--help"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);

    #[cfg(windows)]
    {
        assert!(text.contains(r"\\server\share"), "{text}");
        assert!(text.contains("EcoQoS"), "{text}");
        assert!(text.contains("OneDrive"), "{text}");
        assert!(!text.contains("--nice"), "{text}");
        assert!(!text.contains("ioprio"), "{text}");
    }
    #[cfg(target_os = "macos")]
    {
        assert!(text.contains("~/Library"), "{text}");
        assert!(text.contains("/Volumes"), "{text}");
        assert!(text.contains("macOS は未対応"), "{text}");
        assert!(text.contains("--nice"), "{text}");
        assert!(!text.contains("--skip-cloud-placeholders"), "{text}");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        assert!(text.contains("/mnt/nas"), "{text}");
        assert!(text.contains("ioprio"), "{text}");
        assert!(text.contains("--nice"), "{text}");
        assert!(!text.contains("--skip-cloud-placeholders"), "{text}");
    }
}

#[test]
fn allow_delete_requires_tui() {
    let dir = tempdir().unwrap();
    make_tree(dir.path());
    let root = dir.path().to_string_lossy().into_owned();
    let out = run(&[&root, "--no-tui", "--allow-delete"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--allow-delete"), "{stderr}");
    assert!(stderr.contains("TUI"), "{stderr}");
}

#[test]
fn units_and_sort_can_be_selected() {
    let dir = tempdir().unwrap();
    make_tree(dir.path());
    let root = dir.path().to_string_lossy().into_owned();

    let out = run(&[
        &root, "--no-tui", "--metric", "logical", "--units", "si", "--color", "never",
    ]);
    let text = stdout(&out);
    assert!(text.contains("KB") || text.contains("MB"), "{text}");

    let out = run(&[
        &root, "--format", "flat", "--sort", "name:asc", "--top", "10", "--color", "never",
    ]);
    assert!(out.status.success());
    let text = stdout(&out);
    let root_pos = text.find(&root).unwrap();
    let big_pos = text.find("big").unwrap();
    // 名前昇順では "big" がルート直下の "root.txt" より先に来る
    let root_txt_pos = text.find("root.txt").unwrap();
    assert!(root_pos <= big_pos && big_pos < root_txt_pos, "{text}");
}
