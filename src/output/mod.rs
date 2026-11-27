//! 人間向け出力形式 (report / tree / flat / Markdown / HTML) と JSON/CSV の振り分け。

mod html;
mod markdown;
mod text;

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::io::{self, Write};
use std::path::MAIN_SEPARATOR;

use crate::format::{human_count, pct, sanitize_display, SizeFormat};
use crate::report::ReportMeta;
use crate::scan::engine::ScanOutcome;
use crate::scan::node::{NodeKind, Tree};
use crate::sort::{SortDir, SortField};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Report,
    Tree,
    Flat,
    Markdown,
    Html,
    Json,
    Csv,
}

impl OutputFormat {
    /// 画面やダイアログに出す短い名前。
    pub fn label(self) -> &'static str {
        match self {
            OutputFormat::Report => "概要レポート",
            OutputFormat::Tree => "フォルダ構成",
            OutputFormat::Flat => "一覧",
            OutputFormat::Markdown => "Markdown",
            OutputFormat::Html => "HTML",
            OutputFormat::Json => "JSON",
            OutputFormat::Csv => "CSV",
        }
    }

    /// 保存時の拡張子 (ドットなし)。
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Report | OutputFormat::Tree | OutputFormat::Flat => "txt",
            OutputFormat::Markdown => "md",
            OutputFormat::Html => "html",
            OutputFormat::Json => "json",
            OutputFormat::Csv => "csv",
        }
    }

    /// 明示指定がないときのツリー深さ。
    pub fn default_depth(self) -> usize {
        match self {
            OutputFormat::Tree | OutputFormat::Markdown | OutputFormat::Html => 2,
            _ => 0,
        }
    }

    /// 明示指定がないときの件数。0 は無制限。
    pub fn default_top(self) -> usize {
        match self {
            OutputFormat::Report | OutputFormat::Markdown | OutputFormat::Html => 10,
            OutputFormat::Tree => 20,
            OutputFormat::Flat => 50,
            OutputFormat::Json | OutputFormat::Csv => 0,
        }
    }

    /// 件数 (top) の指定が意味を持つ形式か。
    pub fn uses_top(self) -> bool {
        !matches!(self, OutputFormat::Json | OutputFormat::Csv)
    }

    /// 深さ (depth) の指定が意味を持つ形式か。
    pub fn uses_depth(self) -> bool {
        matches!(
            self,
            OutputFormat::Tree | OutputFormat::Markdown | OutputFormat::Html
        )
    }
}

#[derive(Clone, Debug)]
pub struct OutputOptions {
    pub format: OutputFormat,
    pub depth: usize,
    /// 0 = 無制限
    pub top: usize,
    pub sort_field: SortField,
    pub sort_dir: SortDir,
    pub size: SizeFormat,
    pub ascii: bool,
    pub color: bool,
    pub metric_alloc: bool,
}

impl OutputOptions {
    pub fn metric(&self, node: &crate::scan::node::Node) -> u64 {
        if self.metric_alloc {
            node.total_alloc
        } else {
            node.total_logical
        }
    }

    pub fn metric_label(&self) -> &'static str {
        if self.metric_alloc {
            "ディスク使用量"
        } else {
            "見かけのサイズ"
        }
    }
}

pub fn write_output(
    w: &mut dyn Write,
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
) -> io::Result<()> {
    match opts.format {
        OutputFormat::Report => text::write_report(w, outcome, meta, opts),
        OutputFormat::Tree => {
            let s = text::render_tree(outcome, meta, opts, false);
            w.write_all(s.as_bytes())
        }
        OutputFormat::Flat => text::write_flat(w, outcome, meta, opts),
        OutputFormat::Markdown => markdown::write_markdown(w, outcome, meta, opts),
        OutputFormat::Html => html::write_html(w, outcome, meta, opts),
        OutputFormat::Json => crate::report::write_json(w, outcome, meta),
        OutputFormat::Csv => crate::report::write_csv(w, outcome, meta),
    }
}

// ---------------------------------------------------------------------------
// 共通ヘルパ
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub path: String,
    pub size: u64,
    pub logical: u64,
    pub files: u64,
    pub mtime: i64,
    pub kind: NodeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryFilter {
    Dirs,
    Files,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Ranked {
    size: u64,
    path: String,
    logical: u64,
    files: u64,
    mtime: i64,
    kind: NodeKind,
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.size
            .cmp(&other.size)
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 上位N件をサイズ降順で集める。top=0 は無制限。
pub(crate) fn collect_top(
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    filter: EntryFilter,
    top: usize,
    metric_alloc: bool,
) -> Vec<Entry> {
    let tree = &outcome.tree;
    let root = tree.root;
    let mut parts: Vec<String> = Vec::new();
    let mut stack: Vec<(u32, usize)> = vec![(root, 0)];
    let mut heap: BinaryHeap<std::cmp::Reverse<Ranked>> = BinaryHeap::new();
    let mut unlimited: Vec<Ranked> = Vec::new();

    while let Some((idx, depth)) = stack.pop() {
        parts.truncate(depth);
        let is_root = idx == root;
        if !is_root {
            parts.push(tree.node(idx).name.to_string_lossy().into_owned());
        }
        let node = tree.node(idx);
        let include = match filter {
            EntryFilter::Dirs => node.kind == NodeKind::Dir && !is_root,
            EntryFilter::Files => node.kind == NodeKind::File,
            EntryFilter::All => true,
        };
        if include {
            let size = if metric_alloc {
                node.total_alloc
            } else {
                node.total_logical
            };
            let path = if is_root {
                meta.root.clone()
            } else {
                display_path(&meta.root, &parts)
            };
            let ranked = Ranked {
                size,
                path,
                logical: node.total_logical,
                files: node.file_count,
                mtime: node.mtime,
                kind: node.kind,
            };
            if top == 0 {
                unlimited.push(ranked);
            } else if heap.len() < top {
                heap.push(std::cmp::Reverse(ranked));
            } else if let Some(std::cmp::Reverse(min)) = heap.peek() {
                if ranked.size > min.size {
                    heap.pop();
                    heap.push(std::cmp::Reverse(ranked));
                }
            }
        }

        for &child in node.children.iter().rev() {
            stack.push((child, parts.len()));
        }
    }

    let mut entries: Vec<Ranked> = if top == 0 {
        unlimited
    } else {
        heap.into_iter().map(|std::cmp::Reverse(r)| r).collect()
    };
    entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    entries
        .into_iter()
        .map(|r| Entry {
            path: r.path,
            size: r.size,
            logical: r.logical,
            files: r.files,
            mtime: r.mtime,
            kind: r.kind,
        })
        .collect()
}

/// 全エントリを集める (フラット一覧の並び替え用)。
pub(crate) fn collect_all(
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    metric_alloc: bool,
) -> Vec<Entry> {
    let tree = &outcome.tree;
    let root = tree.root;
    let mut parts: Vec<String> = Vec::new();
    let mut stack: Vec<(u32, usize)> = vec![(root, 0)];
    let mut entries = Vec::new();

    while let Some((idx, depth)) = stack.pop() {
        parts.truncate(depth);
        let is_root = idx == root;
        if !is_root {
            parts.push(tree.node(idx).name.to_string_lossy().into_owned());
        }
        let node = tree.node(idx);
        let size = if metric_alloc {
            node.total_alloc
        } else {
            node.total_logical
        };
        entries.push(Entry {
            path: if is_root {
                meta.root.clone()
            } else {
                display_path(&meta.root, &parts)
            },
            size,
            logical: node.total_logical,
            files: node.file_count,
            mtime: node.mtime,
            kind: node.kind,
        });
        for &child in node.children.iter().rev() {
            stack.push((child, parts.len()));
        }
    }
    entries
}

/// パスを大小文字を無視して比較する (通常はアロケートしない)。
/// 語末 Σ → ς の文脈依存変換があるため、Σ を含む場合は文字列化して比較する。
fn path_case_cmp(a: &str, b: &str) -> Ordering {
    if a.contains('Σ') || b.contains('Σ') {
        return a.to_lowercase().cmp(&b.to_lowercase());
    }
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// フラット一覧の並び替え。top は切り詰め件数 (0 = 無制限)。
pub(crate) fn sort_entries(entries: &mut Vec<Entry>, field: SortField, dir: SortDir, top: usize) {
    entries.sort_by(|a, b| {
        let primary = match field {
            SortField::Size => a.size.cmp(&b.size),
            SortField::Name => path_case_cmp(&a.path, &b.path).then_with(|| a.path.cmp(&b.path)),
            SortField::Count => a.files.cmp(&b.files),
            SortField::Mtime => a.mtime.cmp(&b.mtime),
        };
        let primary = if dir == SortDir::Desc {
            primary.reverse()
        } else {
            primary
        };
        if field == SortField::Name {
            primary
        } else {
            primary.then_with(|| a.path.cmp(&b.path))
        }
    });
    if top > 0 {
        entries.truncate(top);
    }
}

pub(crate) fn root_total(outcome: &ScanOutcome, metric_alloc: bool) -> u64 {
    let node = outcome.tree.node(outcome.tree.root);
    if metric_alloc {
        node.total_alloc
    } else {
        node.total_logical
    }
}

/// ルート表記 + 相対要素から表示用パスを作る。
pub(crate) fn display_path(root: &str, parts: &[String]) -> String {
    if parts.is_empty() {
        return root.to_string();
    }
    let mut out = root.trim_end_matches(['/', '\\']).to_string();
    out.push(MAIN_SEPARATOR);
    out.push_str(&parts.join(std::path::MAIN_SEPARATOR_STR));
    out
}

pub(crate) fn sanitize(s: &str) -> String {
    sanitize_display(s)
}

pub(crate) fn human_bytes(opts: &OutputOptions, bytes: u64) -> String {
    opts.size.format(bytes)
}

pub(crate) fn percent(part: u64, total: u64) -> f64 {
    pct(part, total)
}

pub(crate) fn ansi(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub(crate) fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// `<pre>` 用。改行だけはそのまま残し、それ以外の制御文字を無害化する。
pub(crate) fn html_escape_pre(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push('\n'),
            '\r' => {}
            '\t' => out.push(' '),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Markdown の通常テキスト用。HTML と Markdown の両方を無害化する。
pub(crate) fn md_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() => out.push(' '),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '*' => out.push_str("\\*"),
            '_' => out.push_str("\\_"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            '|' => out.push_str("\\|"),
            '#' => out.push_str("\\#"),
            '!' => out.push_str("\\!"),
            '~' => out.push_str("\\~"),
            c => out.push(c),
        }
    }
    out
}

/// Markdown のインラインコード用。表の区切りや HTML も含めて実体参照にする。
pub(crate) fn md_code(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 13);
    out.push_str("<code>");
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\\' => out.push_str("&#92;"),
            '`' => out.push_str("&#96;"),
            '*' => out.push_str("&#42;"),
            '_' => out.push_str("&#95;"),
            '[' => out.push_str("&#91;"),
            ']' => out.push_str("&#93;"),
            '|' => out.push_str("&#124;"),
            '#' => out.push_str("&#35;"),
            '!' => out.push_str("&#33;"),
            '{' => out.push_str("&#123;"),
            '}' => out.push_str("&#125;"),
            '(' => out.push_str("&#40;"),
            ')' => out.push_str("&#41;"),
            '~' => out.push_str("&#126;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push_str("</code>");
    out
}

fn root_kind_display(kind: &str) -> &str {
    match kind {
        "local" => "ローカル",
        "unc" => "ネットワーク (UNC)",
        "mapped_network" => "ネットワークドライブ",
        other => other,
    }
}

pub(crate) fn count(n: u64) -> String {
    human_count(n)
}

/// ツリーや一覧で利用する、ルートからの割合。
pub(crate) fn sort_children_for(tree: &Tree, parent: u32, opts: &OutputOptions) -> Vec<u32> {
    let mut kids = tree.node(parent).children.clone();
    kids.sort_by(|&a, &b| {
        crate::sort::compare_nodes(
            tree,
            a,
            b,
            opts.sort_field,
            opts.sort_dir,
            opts.metric_alloc,
        )
    });
    kids
}

#[cfg(test)]
mod tests {
    use super::text::{write_flat, write_report};
    use super::*;
    use crate::scan::engine::ScanOutcome;
    use crate::scan::node::{Node, Tree};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixture() -> (ScanOutcome, ReportMeta) {
        let mut tree = Tree::with_root("/root".into(), 0);
        let large = tree
            .push_child_capped(0, Node::dir("large".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(
            large,
            Node::file("a.bin".into(), large, 2, 1000, 1000, 10),
            100,
        )
        .unwrap();
        let small = tree
            .push_child_capped(0, Node::dir("small".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(small, Node::file("b.bin".into(), small, 2, 10, 10, 20), 100)
            .unwrap();
        tree.push_child_capped(0, Node::file("top.bin".into(), 0, 1, 500, 500, 30), 100)
            .unwrap();
        let evil = tree
            .push_child_capped(0, Node::dir("<script>|x".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(evil, Node::file("a|b.txt".into(), evil, 2, 1, 1, 0), 100)
            .unwrap();
        tree.aggregate();

        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 8,
            dirs: 4,
            error_count: 0,
            bytes_alloc: 1511,
            bytes_logical: 1511,
            elapsed: Duration::from_millis(50),
            root: PathBuf::from("/root"),
        };
        let meta = ReportMeta {
            version: "test",
            generated_at: "2026-01-01T00:00:00.000Z".into(),
            root: "/root".into(),
            root_kind: "local",
            profile: "local".into(),
            metric: "allocated",
            allocated_collected: true,
            options: "{}".into(),
            cancelled: false,
            truncated: false,
            threads: 2,
            entries: 8,
            dirs: 4,
            errors: 0,
            elapsed_ms: 50,
        };
        (outcome, meta)
    }

    fn opts(format: OutputFormat) -> OutputOptions {
        OutputOptions {
            format,
            depth: 2,
            top: 10,
            sort_field: SortField::Size,
            sort_dir: SortDir::Desc,
            size: SizeFormat::default(),
            ascii: false,
            color: false,
            metric_alloc: true,
        }
    }

    fn deep_outcome(depth: u16) -> ScanOutcome {
        let mut tree = Tree::with_root("/root".into(), 0);
        let mut cur = 0;
        for i in 0..depth {
            cur = tree
                .push_child_capped(
                    cur,
                    Node::dir(format!("d{i}").into(), cur, i + 1, 0),
                    u64::from(depth) + 10,
                )
                .unwrap();
        }
        tree.push_child_capped(
            cur,
            Node::file("leaf".into(), cur, depth + 1, 1, 1, 0),
            u64::from(depth) + 10,
        )
        .unwrap();
        tree.aggregate();
        ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: u64::from(depth) + 1,
            dirs: u64::from(depth),
            error_count: 0,
            bytes_alloc: 1,
            bytes_logical: 1,
            elapsed: Duration::from_millis(1),
            root: PathBuf::from("/root"),
        }
    }

    fn deep_leaf_path(depth: u16) -> String {
        let mut path = String::from("/root");
        for i in 0..depth {
            path.push(std::path::MAIN_SEPARATOR);
            path.push_str(&format!("d{i}"));
        }
        path.push(std::path::MAIN_SEPARATOR);
        path.push_str("leaf");
        path
    }

    fn preorder_names(tree: &Tree) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![tree.root];
        while let Some(idx) = stack.pop() {
            let n = tree.node(idx);
            out.push(n.name.to_string_lossy().into_owned());
            for &child in n.children.iter().rev() {
                stack.push(child);
            }
        }
        out
    }

    #[derive(serde::Deserialize)]
    struct JsonReport {
        schema_version: u32,
        dskr_version: String,
        generated_at: String,
        root: String,
        root_kind: String,
        profile: String,
        metric: String,
        allocated_collected: bool,
        cancelled: bool,
        truncated: bool,
        incomplete: bool,
        errors_truncated: bool,
        blocked_folders: u64,
        stats: JsonStats,
        options: serde_json::Value,
        errors: Vec<JsonError>,
        tree: JsonNode,
    }

    #[derive(serde::Deserialize)]
    struct JsonStats {
        entries: u64,
        dirs: u64,
        errors: u64,
        elapsed_ms: u64,
        threads: u64,
    }

    #[derive(serde::Deserialize)]
    struct JsonError {
        path: String,
        message: String,
    }

    #[derive(serde::Deserialize)]
    struct JsonNode {
        name: String,
        #[serde(rename = "type")]
        kind: String,
        allocated: u64,
        logical: u64,
        files: u64,
        dirs: u64,
        errors: u64,
        mtime: Option<i64>,
        truncated: bool,
        #[serde(default)]
        children: Vec<JsonNode>,
    }

    fn kind_label(kind: NodeKind) -> &'static str {
        match kind {
            NodeKind::Dir => "dir",
            NodeKind::File => "file",
            NodeKind::Other => "other",
            NodeKind::Blocked => "blocked",
        }
    }

    struct ParsedJson {
        names: Vec<String>,
        max_depth: usize,
        deepest: String,
    }

    fn parse_json_in_large_stack(bytes: &[u8]) -> ParsedJson {
        let text = std::str::from_utf8(bytes)
            .expect("UTF-8 の JSON")
            .to_string();
        std::thread::Builder::new()
            .name("dskr-json-parse".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                use serde::Deserialize;
                // serde_json::from_str は再帰上限を解除できないため、
                // 深い階層では Deserializer::from_str を使う。
                let mut de = serde_json::Deserializer::from_str(&text);
                de.disable_recursion_limit();
                let value = serde_json::Value::deserialize(&mut de).expect("JSON を再読込できる");
                de.end().expect("末尾に余分なデータがない");
                // 64MiB スタック上で Value を破棄する (深い入れ子の drop 対策)。
                let root = value.get("tree").expect("JSON tree がない");
                let mut names = Vec::new();
                let mut max_depth = 0usize;
                let mut deepest = String::new();
                let mut stack = vec![(root, 0usize)];
                while let Some((node, depth)) = stack.pop() {
                    let name = node
                        .get("name")
                        .and_then(|v| v.as_str())
                        .expect("JSON name")
                        .to_string();
                    names.push(name.clone());
                    if depth >= max_depth {
                        max_depth = depth;
                        deepest = name;
                    }
                    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
                        for child in children.iter().rev() {
                            stack.push((child, depth + 1));
                        }
                    }
                }
                ParsedJson {
                    names,
                    max_depth,
                    deepest,
                }
            })
            .unwrap()
            .join()
            .unwrap()
    }

    struct CountingWriter {
        lines: usize,
        current: Vec<u8>,
        last: Vec<u8>,
    }

    impl CountingWriter {
        fn new() -> Self {
            Self {
                lines: 0,
                current: Vec::new(),
                last: Vec::new(),
            }
        }
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            for &b in buf {
                if b == b'\n' {
                    self.lines += 1;
                    self.last = std::mem::take(&mut self.current);
                } else {
                    self.current.push(b);
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn format_defaults_are_consistent() {
        assert_eq!(OutputFormat::Markdown.default_depth(), 2);
        assert_eq!(OutputFormat::Html.default_depth(), 2);
        assert_eq!(OutputFormat::Markdown.default_top(), 10);
        assert_eq!(OutputFormat::Html.default_top(), 10);
        assert_eq!(OutputFormat::Tree.default_top(), 20);
        assert_eq!(OutputFormat::Flat.default_top(), 50);
        assert_eq!(OutputFormat::Json.default_top(), 0);
        assert_eq!(OutputFormat::Json.extension(), "json");
        assert_eq!(OutputFormat::Markdown.extension(), "md");
        assert_eq!(OutputFormat::Html.extension(), "html");
        assert_eq!(OutputFormat::Csv.extension(), "csv");
        assert_eq!(OutputFormat::Report.extension(), "txt");
    }

    #[test]
    fn report_lists_largest_first() {
        let (outcome, meta) = fixture();
        let mut buf = Vec::new();
        write_report(&mut buf, &outcome, &meta, &opts(OutputFormat::Report)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let large = s.find("large").unwrap();
        let small = s.find("small").unwrap();
        assert!(large < small);
        assert!(s.contains("a.bin"));
        assert!(s.contains("dskr レポート"));
    }

    #[test]
    fn tree_respects_depth_and_top() {
        let (outcome, meta) = fixture();
        let mut o = opts(OutputFormat::Tree);
        o.depth = 1;
        let mut buf = Vec::new();
        output_for_test(&mut buf, &outcome, &meta, &o).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("large/"));
        assert!(!s.contains("a.bin"));

        o.depth = 2;
        o.top = 1;
        let mut buf = Vec::new();
        output_for_test(&mut buf, &outcome, &meta, &o).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("a.bin"));
    }

    #[test]
    fn flat_uses_full_paths() {
        let (outcome, meta) = fixture();
        let mut buf = Vec::new();
        write_flat(&mut buf, &outcome, &meta, &opts(OutputFormat::Flat)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        // 区切り文字は OS に従う (Windows は \)
        let sep = MAIN_SEPARATOR;
        assert!(s.contains(&format!("/root{sep}large{sep}a.bin")), "{s}");
        assert!(s.contains(&format!("/root{sep}top.bin")), "{s}");
    }

    #[test]
    fn markdown_escapes_names() {
        let (outcome, meta) = fixture();
        let mut buf = Vec::new();
        output_for_test(&mut buf, &outcome, &meta, &opts(OutputFormat::Markdown)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("&lt;script&gt;"));
        assert!(!s.contains("<script>|x"));
        assert!(s.contains("&#124;"));
    }

    #[test]
    fn markdown_escaping_handles_syntax() {
        assert_eq!(md_text("a*b`c\\d|e[f]"), "a\\*b\\`c\\\\d\\|e\\[f\\]");
        assert_eq!(md_text("<x> & y"), "&lt;x&gt; &amp; y");
        assert_eq!(md_code("a|b"), "<code>a&#124;b</code>");
        assert!(md_code("<script>").starts_with("<code>&lt;script&gt;"));
    }

    #[test]
    fn pre_blocks_keep_line_breaks() {
        for format in [OutputFormat::Markdown, OutputFormat::Html] {
            let (outcome, meta) = fixture();
            let mut o = opts(format);
            o.depth = 2;
            let mut buf = Vec::new();
            output_for_test(&mut buf, &outcome, &meta, &o).unwrap();
            let s = String::from_utf8(buf).unwrap();
            let start = s.find("<pre>").expect("pre block") + "<pre>".len();
            let end = start + s[start..].find("</pre>").expect("pre end");
            let pre = &s[start..end];
            assert!(
                pre.contains('\n'),
                "{format:?} tree should span lines: {pre}"
            );
            assert!(pre.lines().count() >= 2, "{format:?} tree too short: {pre}");
        }
    }

    #[test]
    fn html_is_self_contained_and_escaped() {
        let (outcome, meta) = fixture();
        let mut buf = Vec::new();
        output_for_test(&mut buf, &outcome, &meta, &opts(OutputFormat::Html)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("Content-Security-Policy"));
        assert!(!s.contains("http://"));
        assert!(!s.contains("https://"));
        assert!(!s.contains("<script src"));
        assert!(!s.contains("<link "));
        assert!(s.contains("&lt;script&gt;"));
        assert!(!s.contains("<script>|x"));
    }

    #[test]
    fn html_and_markdown_do_not_emit_external_urls() {
        let (outcome, meta) = fixture();
        for format in [OutputFormat::Html, OutputFormat::Markdown] {
            let mut buf = Vec::new();
            output_for_test(&mut buf, &outcome, &meta, &opts(format)).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert!(!s.contains("http://"));
            assert!(!s.contains("https://"));
        }
    }

    #[test]
    fn blocked_dirs_are_labeled_not_links() {
        // -x 境界などで走査しない実ディレクトリは "(link)" ではなく
        // "(対象外)" と表示される。
        let mut tree = Tree::with_root("/root".into(), 0);
        tree.push_child_capped(0, Node::blocked("other-fs".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(0, Node::other("l".into(), 0, 1, 0, 0, 0), 100)
            .unwrap();
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 2,
            dirs: 0,
            error_count: 0,
            bytes_alloc: 0,
            bytes_logical: 0,
            elapsed: Duration::from_millis(1),
            root: PathBuf::from("/root"),
        };
        let (_, meta) = fixture();
        for format in [OutputFormat::Tree, OutputFormat::Flat] {
            let mut buf = Vec::new();
            output_for_test(&mut buf, &outcome, &meta, &opts(format)).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert!(s.contains("other-fs (対象外)"), "{format:?}: {s}");
        }
        // リンクは従来どおり "(link)"。
        let mut buf = Vec::new();
        output_for_test(&mut buf, &outcome, &meta, &opts(OutputFormat::Tree)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("l (link)"), "{s}");

        for format in [
            OutputFormat::Report,
            OutputFormat::Markdown,
            OutputFormat::Html,
        ] {
            let mut buf = Vec::new();
            output_for_test(&mut buf, &outcome, &meta, &opts(format)).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert!(s.contains("対象外"), "{format:?}: {s}");
        }

        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();
        assert!(s.contains("\"blocked_folders\":1"), "{s}");

        let mut csv = Vec::new();
        crate::report::write_csv(&mut csv, &outcome, &meta).unwrap();
        let s = String::from_utf8(csv).unwrap();
        assert!(s.contains("scan_blocked,schema_version"), "{s}");
    }

    #[test]
    fn incomplete_scan_is_announced_in_all_formats() {
        let (mut outcome, mut meta) = fixture();
        outcome.error_count = 1;
        outcome.errors.push(crate::scan::engine::ScanError {
            path: "/root/denied".into(),
            message: "permission denied".into(),
        });
        meta.errors = 1;

        for format in [
            OutputFormat::Report,
            OutputFormat::Markdown,
            OutputFormat::Html,
        ] {
            let mut buf = Vec::new();
            output_for_test(&mut buf, &outcome, &meta, &opts(format)).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert!(s.contains("不完全"), "{format:?}: {s}");
        }

        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();
        assert!(s.contains("\"incomplete\":true"), "{s}");
    }

    #[test]
    fn truncated_error_list_is_announced() {
        let (mut outcome, mut meta) = fixture();
        for i in 0..2 {
            outcome.errors.push(crate::scan::engine::ScanError {
                path: format!("/root/denied{i}"),
                message: "permission denied".into(),
            });
        }
        outcome.error_count = 1005;
        meta.errors = 1005;

        for format in [OutputFormat::Markdown, OutputFormat::Html] {
            let mut buf = Vec::new();
            output_for_test(&mut buf, &outcome, &meta, &opts(format)).unwrap();
            let s = String::from_utf8(buf).unwrap();
            assert!(s.contains("先頭 2 件"), "{format:?}: {s}");
        }

        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();
        assert!(s.contains("\"errors_truncated\":true"), "{s}");
    }

    #[test]
    fn csv_includes_scan_summary() {
        let (outcome, mut meta) = fixture();
        meta.cancelled = true;
        meta.truncated = true;
        meta.errors = 2;

        let mut buf = Vec::new();
        output_for_test(&mut buf, &outcome, &meta, &opts(OutputFormat::Csv)).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let mut lines = s.lines();
        assert!(
            lines.next().unwrap().ends_with(
                "truncated,cancelled,scan_truncated,scan_errors,allocated_collected,scan_blocked,schema_version"
            ),
            "{s}"
        );
        let root = lines.next().unwrap();
        assert!(root.ends_with(",false,true,true,2,true,0,1"), "{root}");
    }

    #[test]
    fn json_uses_null_for_unknown_mtime() {
        let (outcome, meta) = fixture();
        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();
        assert!(s.contains("\"mtime\":null"), "{s}");
    }

    #[test]
    fn json_round_trip_validates_schema_types_and_order() {
        let (outcome, meta) = fixture();
        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();

        let report: JsonReport = serde_json::from_str(&s).expect("JSON を全体再解析できる");
        assert_eq!(report.schema_version, crate::report::SCHEMA_VERSION);
        assert_eq!(report.dskr_version, "test");
        assert_eq!(report.generated_at, "2026-01-01T00:00:00.000Z");
        assert_eq!(report.root, "/root");
        assert_eq!(report.root_kind, "local");
        assert_eq!(report.profile, "local");
        assert_eq!(report.metric, "allocated");
        assert!(report.allocated_collected);
        assert!(!report.cancelled);
        assert!(!report.truncated);
        assert!(!report.incomplete);
        assert!(!report.errors_truncated);
        assert_eq!(report.blocked_folders, 0);
        assert_eq!(report.stats.entries, meta.entries);
        assert_eq!(report.stats.dirs, meta.dirs);
        assert_eq!(report.stats.errors, meta.errors);
        assert_eq!(report.stats.elapsed_ms, meta.elapsed_ms as u64);
        assert_eq!(report.stats.threads, meta.threads as u64);
        assert!(report.options.is_object(), "{:?}", report.options);
        assert!(report.errors.is_empty());
        assert_eq!(report.tree.name, "/root");
        assert_eq!(report.tree.kind, "dir");
        assert_eq!(report.tree.mtime, None);

        // 親子関係と兄弟の順序を Tree と突き合わせる。
        let mut stack = vec![(&report.tree, outcome.tree.root)];
        let mut seen = 0usize;
        while let Some((jn, idx)) = stack.pop() {
            let n = outcome.tree.node(idx);
            seen += 1;
            assert_eq!(jn.name, n.name.to_string_lossy());
            assert_eq!(jn.kind, kind_label(n.kind));
            assert_eq!(jn.allocated, n.total_alloc);
            assert_eq!(jn.logical, n.total_logical);
            assert_eq!(jn.files, n.file_count);
            assert_eq!(jn.dirs, n.dir_count);
            assert_eq!(jn.errors, u64::from(n.err_count));
            assert_eq!(jn.mtime, if n.mtime == 0 { None } else { Some(n.mtime) });
            assert_eq!(jn.truncated, n.truncated);
            assert_eq!(jn.children.len(), n.children.len());
            for (child_json, &child_idx) in jn.children.iter().zip(&n.children) {
                stack.push((child_json, child_idx));
            }
        }
        assert_eq!(seen, outcome.tree.nodes.len());
    }

    #[test]
    fn json_round_trip_escapes_unicode_newline_quote_and_backslash() {
        let names = [
            "空 白",
            "line\nbreak",
            "quote\"x",
            "back\\slash",
            "tab\tname",
            "ctrl\u{1}name",
        ];
        let mut tree = Tree::with_root("/root".into(), 0);
        for (i, name) in names.iter().enumerate() {
            tree.push_child_capped(0, Node::file((*name).into(), 0, 1, 1, 1, i as i64), 100)
                .unwrap();
        }
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: names.len() as u64,
            dirs: 0,
            error_count: 0,
            bytes_alloc: names.len() as u64,
            bytes_logical: names.len() as u64,
            elapsed: Duration::from_millis(1),
            root: PathBuf::from("/root"),
        };
        let (_, meta) = fixture();
        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();

        let report: JsonReport =
            serde_json::from_str(&s).expect("特殊文字を含む JSON を再解析できる");
        let actual: Vec<&str> = report
            .tree
            .children
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(actual, names);

        assert!(s.contains("\\n"), "改行をエスケープする: {s}");
        assert!(s.contains("\\\""), "引用符をエスケープする: {s}");
        assert!(s.contains("\\\\"), "バックスラッシュをエスケープする: {s}");
        assert!(s.contains("\\t"), "タブをエスケープする: {s}");
        assert!(s.contains("\\u0001"), "制御文字をエスケープする: {s}");
    }

    #[test]
    fn json_round_trip_reports_incomplete_and_truncated_errors() {
        let (mut outcome, mut meta) = fixture();
        outcome.error_count = 1005;
        outcome.errors.push(crate::scan::engine::ScanError {
            path: "/root/a\nb".into(),
            message: "quote\"\\".into(),
        });
        outcome.errors.push(crate::scan::engine::ScanError {
            path: "/root/denied".into(),
            message: "permission denied".into(),
        });
        meta.errors = 1005;
        meta.truncated = true;
        meta.cancelled = true;

        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        let s = String::from_utf8(json).unwrap();
        let report: JsonReport = serde_json::from_str(&s).expect("エラー情報を再解析できる");

        assert!(report.incomplete);
        assert!(report.errors_truncated);
        assert!(report.truncated);
        assert!(report.cancelled);
        assert_eq!(report.stats.errors, 1005);
        assert_eq!(report.errors.len(), 2);
        assert_eq!(report.errors[0].path, "/root/a\nb");
        assert_eq!(report.errors[0].message, "quote\"\\");
        assert_eq!(report.errors[1].path, "/root/denied");
    }

    #[test]
    fn deep_json_full_parse_reaches_deepest_node() {
        let (_, meta) = fixture();
        let outcome = deep_outcome(10_000);
        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        assert!(json.ends_with(b"}\n"));

        let expected = preorder_names(&outcome.tree);
        let actual = parse_json_in_large_stack(&json);
        assert_eq!(actual.names.len(), 10_002);
        assert_eq!(actual.names, expected);
        assert_eq!(actual.names.last().map(String::as_str), Some("leaf"));
        assert_eq!(actual.max_depth, 10_001);
        assert_eq!(actual.deepest, "leaf");
    }

    #[test]
    fn deep_csv_keeps_paths_and_order() {
        let (_, meta) = fixture();
        let sep = std::path::MAIN_SEPARATOR;

        let depth = 2_000;
        let outcome = deep_outcome(depth);
        let mut csv = Vec::new();
        crate::report::write_csv(&mut csv, &outcome, &meta).unwrap();
        let s = String::from_utf8(csv).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), usize::from(depth) + 3); // header + root + dirs + leaf
        assert!(lines[0].starts_with("path,"));
        assert!(lines[1].starts_with("/root,true,"), "{}", lines[1]);
        assert!(
            lines[2].starts_with(&format!("/root{sep}d0,true,")),
            "{}",
            lines[2]
        );
        let leaf = deep_leaf_path(depth);
        assert!(
            lines.last().unwrap().starts_with(&leaf),
            "{}",
            lines.last().unwrap()
        );

        // 10,000 階層は出力が巨大になるため、破棄 Writer で行数と最深行だけ確認する。
        let outcome = deep_outcome(10_000);
        let mut sink = CountingWriter::new();
        crate::report::write_csv(&mut sink, &outcome, &meta).unwrap();
        assert_eq!(sink.lines, 10_003);
        let last = String::from_utf8(sink.last).unwrap();
        assert!(last.starts_with(&deep_leaf_path(10_000)), "{last}");
    }

    #[test]
    fn deep_tree_with_many_siblings_is_preorder_in_json_and_csv() {
        let (_, meta) = fixture();
        let depth = 1_000u16;
        let siblings = 5u16;
        let mut tree = Tree::with_root("/root".into(), 0);
        let mut cur = 0;
        for i in 0..depth {
            cur = tree
                .push_child_capped(
                    cur,
                    Node::dir(format!("d{i}").into(), cur, i + 1, 0),
                    u64::from(depth) * u64::from(siblings + 2),
                )
                .unwrap();
            for j in 0..siblings {
                tree.push_child_capped(
                    cur,
                    Node::file(format!("s{i}-{j}").into(), cur, i + 1, 1, 1, 0),
                    u64::from(depth) * u64::from(siblings + 2),
                )
                .unwrap();
            }
        }
        tree.push_child_capped(
            cur,
            Node::file("leaf".into(), cur, depth + 1, 1, 1, 0),
            u64::from(depth) * u64::from(siblings + 2),
        )
        .unwrap();
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: u64::from(depth) * u64::from(siblings + 1) + 1,
            dirs: u64::from(depth),
            error_count: 0,
            bytes_alloc: 1,
            bytes_logical: 1,
            elapsed: Duration::from_millis(1),
            root: PathBuf::from("/root"),
        };

        let expected = preorder_names(&outcome.tree);
        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        assert_eq!(parse_json_in_large_stack(&json).names, expected);

        let mut csv = Vec::new();
        crate::report::write_csv(&mut csv, &outcome, &meta).unwrap();
        let s = String::from_utf8(csv).unwrap();
        assert_eq!(s.lines().count(), expected.len() + 1);
    }

    #[test]
    fn json_and_csv_escape_unicode_and_special_names() {
        let (_, meta) = fixture();
        let mut tree = Tree::with_root("/root".into(), 0);
        let dir = tree
            .push_child_capped(0, Node::dir("空 白,quote\"x".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(dir, Node::file("a,b\"c".into(), dir, 2, 1, 1, 0), 100)
            .unwrap();
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 3,
            dirs: 1,
            error_count: 0,
            bytes_alloc: 1,
            bytes_logical: 1,
            elapsed: Duration::from_millis(1),
            root: PathBuf::from("/root"),
        };

        let mut json = Vec::new();
        crate::report::write_json(&mut json, &outcome, &meta).unwrap();
        assert_eq!(
            parse_json_in_large_stack(&json).names,
            vec![
                "/root".to_string(),
                "空 白,quote\"x".to_string(),
                "a,b\"c".to_string()
            ]
        );

        let mut csv = Vec::new();
        crate::report::write_csv(&mut csv, &outcome, &meta).unwrap();
        let s = String::from_utf8(csv).unwrap();
        let escaped = |path: &str| format!("\"{}\"", path.replace('"', "\"\""));
        let sep = std::path::MAIN_SEPARATOR;
        let dir_path = format!("/root{sep}空 白,quote\"x");
        assert!(s.contains(&format!("{},true,", escaped(&dir_path))), "{s}");
        let file_path = format!("{dir_path}{sep}a,b\"c");
        assert!(
            s.contains(&format!("{},false,", escaped(&file_path))),
            "{s}"
        );
    }

    fn output_for_test(
        w: &mut dyn Write,
        outcome: &ScanOutcome,
        meta: &ReportMeta,
        opts: &OutputOptions,
    ) -> io::Result<()> {
        write_output(w, outcome, meta, opts)
    }
}
