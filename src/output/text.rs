use std::io::{self, Write};
use std::path::MAIN_SEPARATOR;

use super::{
    ansi, collect_all, collect_top, count, human_bytes, percent, root_kind_display, sanitize,
    sort_children_for, sort_entries, EntryFilter, OutputOptions,
};
use crate::report::ReportMeta;
use crate::scan::engine::ScanOutcome;
use crate::scan::node::{NodeKind, Tree};
use crate::sort::{SortDir, SortField};

pub(crate) fn write_report(
    w: &mut dyn Write,
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
) -> io::Result<()> {
    let root = outcome.tree.node(outcome.tree.root);
    let total = opts.metric(root);
    let color = opts.color;

    writeln!(w, "{}", ansi("dskr レポート", "1", color))?;
    writeln!(w, "対象       : {}", sanitize(&meta.root))?;
    writeln!(w, "種類       : {}", root_kind_display(meta.root_kind))?;
    writeln!(w, "生成日時   : {}", meta.generated_at)?;
    writeln!(
        w,
        "走査       : {} 項目 / {} フォルダ / {:.2} 秒 / {} スレッド",
        count(meta.entries),
        count(meta.dirs),
        meta.elapsed_ms as f64 / 1000.0,
        meta.threads
    )?;
    writeln!(
        w,
        "合計       : {} ({}) / {} (見かけのサイズ)",
        human_bytes(opts, total),
        opts.metric_label(),
        human_bytes(opts, root.total_logical)
    )?;
    writeln!(
        w,
        "ファイル   : {}   フォルダ: {}   エラー: {}",
        count(root.file_count),
        count(root.dir_count),
        count(meta.errors)
    )?;
    if meta.cancelled {
        writeln!(
            w,
            "{}",
            ansi("注意: 走査は中止されました (途中までの結果)", "33", color)
        )?;
    }
    if meta.truncated {
        writeln!(
            w,
            "{}",
            ansi(
                "注意: 深さ/件数の上限により一部が省略されています",
                "33",
                color
            )
        )?;
    }

    let dirs = collect_top(
        outcome,
        meta,
        EntryFilter::Dirs,
        opts.top,
        opts.metric_alloc,
    );
    writeln!(w)?;
    writeln!(
        w,
        "{}",
        ansi(
            &format!("容量の大きいフォルダ ({}):", dirs.len()),
            "1",
            color
        )
    )?;
    if dirs.is_empty() {
        writeln!(w, "  (なし)")?;
    }
    for e in &dirs {
        writeln!(
            w,
            "  {:>12}  {:>5.1}%  {:>10}  {}",
            human_bytes(opts, e.size),
            percent(e.size, total),
            count(e.files),
            ansi(&sanitize(&e.path), "36", color)
        )?;
    }

    let files = collect_top(
        outcome,
        meta,
        EntryFilter::Files,
        opts.top,
        opts.metric_alloc,
    );
    writeln!(w)?;
    writeln!(
        w,
        "{}",
        ansi(
            &format!("容量の大きいファイル ({}):", files.len()),
            "1",
            color
        )
    )?;
    if files.is_empty() {
        writeln!(w, "  (なし)")?;
    }
    for e in &files {
        writeln!(
            w,
            "  {:>12}  {:>5.1}%  {}",
            human_bytes(opts, e.size),
            percent(e.size, total),
            sanitize(&e.path)
        )?;
    }

    if !outcome.errors.is_empty() {
        let limit = if opts.top == 0 {
            usize::MAX
        } else {
            opts.top.max(1)
        };
        writeln!(w)?;
        writeln!(
            w,
            "{}",
            ansi(
                &format!(
                    "エラー ({} 件中 {} 件を表示):",
                    meta.errors,
                    outcome.errors.len().min(limit)
                ),
                "1",
                color
            )
        )?;
        for e in outcome.errors.iter().take(limit) {
            writeln!(
                w,
                "  {}: {}",
                ansi(&sanitize(&e.path), "33", color),
                sanitize(&e.message)
            )?;
        }
    }

    Ok(())
}

pub(crate) fn write_flat(
    w: &mut dyn Write,
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
) -> io::Result<()> {
    let total = super::root_total(outcome, opts.metric_alloc);
    // 既定のサイズ降順は上位N件を有界ヒープで集める。それ以外は全件集めて並び替える。
    let mut entries =
        if opts.sort_field == SortField::Size && opts.sort_dir == SortDir::Desc && opts.top > 0 {
            collect_top(outcome, meta, EntryFilter::All, opts.top, opts.metric_alloc)
        } else {
            let mut all = collect_all(outcome, meta, opts.metric_alloc);
            sort_entries(&mut all, opts.sort_field, opts.sort_dir, opts.top);
            all
        };
    entries.shrink_to_fit();
    for e in entries {
        let mut path = sanitize(&e.path);
        match e.kind {
            NodeKind::Dir => {
                if !path.ends_with(['/', '\\']) {
                    path.push(MAIN_SEPARATOR);
                }
            }
            NodeKind::Other => path.push_str(" (link)"),
            NodeKind::Blocked => path.push_str(" (対象外)"),
            NodeKind::File => {}
        }
        let path = if opts.color && e.kind == NodeKind::Dir {
            ansi(&path, "36", true)
        } else {
            path
        };
        writeln!(
            w,
            "{:>12}  {:>5.1}%  {}",
            human_bytes(opts, e.size),
            percent(e.size, total),
            path
        )?;
    }
    Ok(())
}

pub(crate) fn render_tree(
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
    color: bool,
) -> String {
    let tree = &outcome.tree;
    let root_idx = tree.root;
    let total = super::root_total(outcome, opts.metric_alloc);
    let mut out = String::new();
    let root = tree.node(root_idx);
    out.push_str(&format!(
        "{}  {}  {} ファイル\n",
        ansi(&sanitize(&meta.root), "1", color),
        human_bytes(opts, opts.metric(root)),
        count(root.file_count)
    ));
    if opts.depth > 0 {
        render_children(&mut out, tree, root_idx, "", 1, total, opts, color);
    }
    out
}

fn glyphs(ascii: bool) -> (&'static str, &'static str, &'static str, &'static str) {
    if ascii {
        ("|-- ", "`-- ", "|   ", "    ")
    } else {
        ("├─ ", "└─ ", "│   ", "    ")
    }
}

#[allow(clippy::too_many_arguments)]
fn render_children(
    out: &mut String,
    tree: &Tree,
    parent: u32,
    prefix: &str,
    depth: usize,
    total: u64,
    opts: &OutputOptions,
    color: bool,
) {
    if opts.depth == 0 || depth > opts.depth {
        return;
    }
    let (branch, last, vertical, space) = glyphs(opts.ascii);
    let mut kids = sort_children_for(tree, parent, opts);
    let total_kids = kids.len();
    if opts.top > 0 {
        kids.truncate(opts.top);
    }
    let shown = kids.len();

    for (i, &idx) in kids.iter().enumerate() {
        let is_last = i + 1 == shown && shown == total_kids;
        let connector = if is_last { last } else { branch };
        let child_prefix = format!("{prefix}{}", if is_last { space } else { vertical });
        let node = tree.node(idx);
        let name = display_name(node);
        let size = opts.metric(node);
        let pct = percent(size, total);
        let kind_color = match node.kind {
            NodeKind::Dir => "36",
            NodeKind::Other | NodeKind::Blocked => "2",
            NodeKind::File => "0",
        };
        let label = if color {
            ansi(&name, kind_color, true)
        } else {
            name
        };
        let mut extra = String::new();
        if node.kind == NodeKind::Dir {
            extra = format!("  ({} ファイル)", count(node.file_count));
        }
        out.push_str(&format!(
            "{prefix}{connector}{}  {:>10}  {:>5.1}%{}\n",
            label,
            human_bytes(opts, size),
            pct,
            extra
        ));

        if node.kind == NodeKind::Dir && depth < opts.depth {
            render_children(out, tree, idx, &child_prefix, depth + 1, total, opts, color);
        }
    }

    if shown < total_kids {
        out.push_str(&format!(
            "{prefix}{}... ({} more)\n",
            last,
            total_kids - shown
        ));
    }
}

fn display_name(node: &crate::scan::node::Node) -> String {
    let mut name = sanitize(&node.name.to_string_lossy());
    match node.kind {
        NodeKind::Dir => name.push('/'),
        NodeKind::Other => name.push_str(" (link)"),
        NodeKind::Blocked => name.push_str(" (対象外)"),
        NodeKind::File => {}
    }
    name
}
