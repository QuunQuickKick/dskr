//! JSON / CSV レポートの書き出し (ストリーミング、追加メモリなし)。

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scan::engine::ScanOutcome;
use crate::scan::node::{NodeKind, Tree};
use crate::timefmt::civil_from_days;

/// JSON / CSV の公開スキーマバージョン。
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub struct ReportMeta {
    pub version: &'static str,
    pub generated_at: String,
    pub root: String,
    pub root_kind: &'static str,
    pub profile: String,
    pub metric: &'static str,
    /// allocated (ディスク使用量) を実際に取得したか。
    /// Windows のネットワークプロファイルでは false になり得る。
    pub allocated_collected: bool,
    pub options: String, // JSON オブジェクト文字列
    pub cancelled: bool,
    pub truncated: bool,
    pub threads: usize,
    pub entries: u64,
    pub dirs: u64,
    pub errors: u64,
    pub elapsed_ms: u128,
}

fn jstr(s: &str) -> String {
    json_string(s)
}

/// JSON 文字列リテラルへ安全にエスケープする。
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// スキャン結果からレポートのメタデータを組み立てる。
/// headless 出力 (main.rs) と TUI 保存 (app.rs) の共通処理。
#[allow(clippy::too_many_arguments)]
pub fn build_meta(
    outcome: &ScanOutcome,
    root: &str,
    root_kind: &'static str,
    profile: &str,
    threads: usize,
    options_json: &str,
    metric: &'static str,
    allocated_collected: bool,
) -> ReportMeta {
    ReportMeta {
        version: env!("CARGO_PKG_VERSION"),
        generated_at: iso_now(),
        root: root.to_string(),
        root_kind,
        profile: profile.to_string(),
        metric,
        allocated_collected,
        options: options_json.to_string(),
        cancelled: outcome.cancelled,
        truncated: outcome.truncated,
        threads,
        entries: outcome.entries,
        dirs: outcome.dirs,
        errors: outcome.error_count,
        elapsed_ms: outcome.elapsed.as_millis(),
    }
}

pub fn write_json(w: &mut dyn Write, outcome: &ScanOutcome, meta: &ReportMeta) -> io::Result<()> {
    let errors_truncated = meta.errors > outcome.errors.len() as u64;
    let blocked = outcome.tree.blocked_count();
    write!(
        w,
        "{{\"schema_version\":{},\"dskr_version\":{},\"generated_at\":{},\"root\":{},\"root_kind\":{},\"profile\":{},\"metric\":{},\"allocated_collected\":{},\"cancelled\":{},\"truncated\":{},\"incomplete\":{},\"errors_truncated\":{},\"blocked_folders\":{},\"stats\":{{\"entries\":{},\"dirs\":{},\"errors\":{},\"elapsed_ms\":{},\"threads\":{}}},\"options\":{},\"errors\":[",
        SCHEMA_VERSION,
        jstr(meta.version),
        jstr(&meta.generated_at),
        jstr(&meta.root),
        jstr(meta.root_kind),
        jstr(&meta.profile),
        jstr(meta.metric),
        meta.allocated_collected,
        meta.cancelled,
        meta.truncated,
        meta.errors > 0,
        errors_truncated,
        blocked,
        meta.entries,
        meta.dirs,
        meta.errors,
        meta.elapsed_ms,
        meta.threads,
        meta.options,
    )?;
    for (i, e) in outcome.errors.iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write!(
            w,
            "{{\"path\":{},\"message\":{}}}",
            jstr(&e.path),
            jstr(&e.message)
        )?;
    }
    w.write_all(b"],\"tree\":")?;
    node_json(w, &outcome.tree, outcome.tree.root)?;
    w.write_all(b"}\n")?;
    Ok(())
}

fn node_json(w: &mut dyn Write, t: &Tree, i: u32) -> io::Result<()> {
    let n = &t.nodes[i as usize];
    let ty = match n.kind {
        NodeKind::Dir => "dir",
        NodeKind::File => "file",
        NodeKind::Other => "other",
        NodeKind::Blocked => "blocked",
    };
    // mtime 0 は取得できなかったことを表す (UI も "-" 表示)。
    // JSON では 1970年と誤認しないよう null にする。
    let mtime = if n.mtime == 0 {
        "null".to_string()
    } else {
        n.mtime.to_string()
    };
    write!(
        w,
        "{{\"name\":{},\"type\":{},\"allocated\":{},\"logical\":{},\"files\":{},\"dirs\":{},\"errors\":{},\"mtime\":{},\"truncated\":{}",
        jstr(&n.name.to_string_lossy()),
        jstr(ty),
        n.total_alloc,
        n.total_logical,
        n.file_count,
        n.dir_count,
        n.err_count,
        mtime,
        n.truncated,
    )?;
    if n.kind == NodeKind::Dir && !n.children.is_empty() {
        w.write_all(b",\"children\":[")?;
        for (k, &c) in n.children.iter().enumerate() {
            if k > 0 {
                w.write_all(b",")?;
            }
            node_json(w, t, c)?;
        }
        w.write_all(b"]")?;
    }
    w.write_all(b"}")?;
    Ok(())
}

fn csv_field(s: &str) -> String {
    // スプレッドシートの数式インジェクション対策:
    // 先頭が = + - @ タブ CR の場合は無害な接頭辞を付ける。
    let mut v = String::with_capacity(s.len() + 1);
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        v.push('\'');
    }
    // 端末エスケープ注入対策: 改行・タブ以外の制御文字を無害化する
    for c in s.chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            v.push('?');
        } else {
            v.push(c);
        }
    }
    if v.contains(',') || v.contains('"') || v.contains('\n') || v.contains('\r') {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v
    }
}

/// レポート用のファイルを安全に開く。
///
/// - Unix では新規作成時のパーミッションを 0600 にする (ファイル名の漏洩防止)
/// - `exclusive` のときは O_EXCL で開き、既存のシンボリックリンクを辿らない
pub fn create_private(path: &std::path::Path, exclusive: bool) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true);
    if exclusive {
        opts.create_new(true);
    } else {
        opts.create(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 既存のシンボリックリンクを辿らない (書込み先のすり替え防止)
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // 再解析ポイント自体を開き、リンク先への書込みを防ぐ
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    opts.open(path)
}

pub fn write_csv(w: &mut dyn Write, outcome: &ScanOutcome, meta: &ReportMeta) -> io::Result<()> {
    let blocked = outcome.tree.blocked_count();
    writeln!(
        w,
        "path,is_dir,allocated,logical,file_count,dir_count,error_count,truncated,cancelled,scan_truncated,scan_errors,allocated_collected,scan_blocked,schema_version"
    )?;
    write_csv_node(
        w,
        &outcome.tree,
        outcome.tree.root,
        &mut Vec::new(),
        meta.cancelled,
        meta.truncated,
        meta.errors,
        meta.allocated_collected,
        blocked,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_csv_node(
    w: &mut dyn Write,
    t: &Tree,
    i: u32,
    parts: &mut Vec<String>,
    cancelled: bool,
    scan_truncated: bool,
    scan_errors: u64,
    allocated_collected: bool,
    scan_blocked: u64,
) -> io::Result<()> {
    let n = &t.nodes[i as usize];
    let name = n.name.to_string_lossy().into_owned();
    parts.push(name);
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let p = parts.join(&sep);
    writeln!(
        w,
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        csv_field(&p),
        n.kind == NodeKind::Dir,
        n.total_alloc,
        n.total_logical,
        n.file_count,
        n.dir_count,
        n.err_count,
        n.truncated,
        cancelled,
        scan_truncated,
        scan_errors,
        allocated_collected,
        scan_blocked,
        SCHEMA_VERSION,
    )?;
    let depth = parts.len();
    for &c in &n.children {
        write_csv_node(
            w,
            t,
            c,
            parts,
            cancelled,
            scan_truncated,
            scan_errors,
            allocated_collected,
            scan_blocked,
        )?;
        parts.truncate(depth);
    }
    parts.pop();
    Ok(())
}

/// UTC の ISO-8601 文字列。
pub fn iso_now() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let millis = d.subsec_millis();
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// レポートファイル名に使うタイムスタンプ。
pub fn timestamp_slug() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{day:02}-{:02}{:02}{:02}",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape() {
        assert_eq!(jstr("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(jstr("日本語"), "\"日本語\"");
        assert_eq!(jstr("\u{1}"), "\"\\u0001\"");
        assert_eq!(jstr("\u{7f}"), "\"\\u007f\"");
    }

    #[test]
    fn csv_escape() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        // 数式インジェクション対策
        assert_eq!(csv_field("=cmd|calc"), "'=cmd|calc");
        assert_eq!(csv_field("+1"), "'+1");
        assert_eq!(csv_field("@x"), "'@x");
        assert_eq!(csv_field("-x,y"), "\"'-x,y\"");
        // 制御文字 (端末エスケープ) は無害化
        assert_eq!(csv_field("a\u{1b}]52;c\u{7}"), "a?]52;c?");
    }

    #[test]
    fn time_helpers() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert!(iso_now().starts_with("20"));
        assert_eq!(timestamp_slug().len(), 15);
    }

    #[cfg(unix)]
    #[test]
    fn private_report_file_does_not_follow_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"original").unwrap();
        let link = dir.path().join("report.json");
        symlink(&target, &link).unwrap();

        // exclusive 指定では既存シンボリックリンクを辿らずエラーになる
        assert!(create_private(&link, true).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        // 非 exclusive でもシンボリックリンクは辿らない
        assert!(create_private(&link, false).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");

        // 非 exclusive で新規作成したファイルは 0600
        let fresh = dir.path().join("fresh.json");
        let f = create_private(&fresh, false).unwrap();
        drop(f);
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
