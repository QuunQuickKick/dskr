use std::io::{self, Write};

use super::text::render_tree;
use super::{
    collect_top, count, html_escape_pre, human_bytes, md_code, md_text, percent, root_kind_display,
    EntryFilter, OutputOptions,
};
use crate::report::ReportMeta;
use crate::scan::engine::ScanOutcome;

pub(crate) fn write_markdown(
    w: &mut dyn Write,
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
) -> io::Result<()> {
    let root = outcome.tree.node(outcome.tree.root);
    let total = opts.metric(root);

    writeln!(w, "# ディスク使用量レポート: {}", md_code(&meta.root))?;
    writeln!(w)?;
    writeln!(w, "- 生成日時: {}", meta.generated_at)?;
    writeln!(
        w,
        "- 対象: {} ({})",
        md_code(&meta.root),
        root_kind_display(meta.root_kind)
    )?;
    writeln!(
        w,
        "- 走査: {} 項目 / {} フォルダ / {:.2} 秒 / {} スレッド",
        count(meta.entries),
        count(meta.dirs),
        meta.elapsed_ms as f64 / 1000.0,
        meta.threads
    )?;
    if meta.cancelled {
        writeln!(w, "- **注意: 走査は中止されました (途中までの結果)**")?;
    }
    if meta.truncated {
        writeln!(w, "- **注意: 深さ/件数の上限により一部が省略されています**")?;
    }
    writeln!(w)?;

    writeln!(w, "## サマリー")?;
    writeln!(w)?;
    writeln!(w, "| 項目 | 値 |")?;
    writeln!(w, "|---|---:|")?;
    writeln!(
        w,
        "| 合計 ({}) | {} |",
        opts.metric_label(),
        human_bytes(opts, total)
    )?;
    writeln!(
        w,
        "| 合計 (見かけのサイズ) | {} |",
        human_bytes(opts, root.total_logical)
    )?;
    writeln!(w, "| ファイル数 | {} |", count(root.file_count))?;
    writeln!(w, "| フォルダ数 | {} |", count(root.dir_count))?;
    writeln!(w, "| エラー数 | {} |", count(meta.errors))?;
    writeln!(w)?;

    let dirs = collect_top(
        outcome,
        meta,
        EntryFilter::Dirs,
        opts.top,
        opts.metric_alloc,
    );
    writeln!(w, "## 容量の大きいフォルダ ({})", dirs.len())?;
    writeln!(w)?;
    if dirs.is_empty() {
        writeln!(w, "(なし)")?;
    } else {
        writeln!(w, "| 割合 | サイズ | ファイル数 | パス |")?;
        writeln!(w, "|---:|---:|---:|---|")?;
        for e in &dirs {
            writeln!(
                w,
                "| {:.1}% | {} | {} | {} |",
                percent(e.size, total),
                human_bytes(opts, e.size),
                count(e.files),
                md_code(&e.path)
            )?;
        }
    }
    writeln!(w)?;

    let files = collect_top(
        outcome,
        meta,
        EntryFilter::Files,
        opts.top,
        opts.metric_alloc,
    );
    writeln!(w, "## 容量の大きいファイル ({})", files.len())?;
    writeln!(w)?;
    if files.is_empty() {
        writeln!(w, "(なし)")?;
    } else {
        writeln!(w, "| 割合 | サイズ | パス |")?;
        writeln!(w, "|---:|---:|---|")?;
        for e in &files {
            writeln!(
                w,
                "| {:.1}% | {} | {} |",
                percent(e.size, total),
                human_bytes(opts, e.size),
                md_code(&e.path)
            )?;
        }
    }
    writeln!(w)?;

    if !outcome.errors.is_empty() {
        writeln!(w, "## エラー")?;
        writeln!(w)?;
        writeln!(w, "| パス | 内容 |")?;
        writeln!(w, "|---|---|")?;
        for e in &outcome.errors {
            writeln!(w, "| {} | {} |", md_code(&e.path), md_text(&e.message))?;
        }
        writeln!(w)?;
    }

    if opts.depth > 0 {
        let tree = render_tree(outcome, meta, opts, false);
        writeln!(w, "<details>")?;
        writeln!(w, "<summary>フォルダ構成 (深さ {})</summary>", opts.depth)?;
        writeln!(w)?;
        writeln!(w, "<pre>")?;
        writeln!(w, "{}", html_escape_pre(tree.trim_end()))?;
        writeln!(w, "</pre>")?;
        writeln!(w)?;
        writeln!(w, "</details>")?;
    }

    Ok(())
}
