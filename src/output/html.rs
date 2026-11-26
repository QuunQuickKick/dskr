use std::io::{self, Write};

use super::text::render_tree;
use super::{
    collect_top, count, html_escape, html_escape_pre, human_bytes, percent, root_kind_display,
    EntryFilter, OutputOptions,
};
use crate::report::ReportMeta;
use crate::scan::engine::ScanOutcome;

const STYLE: &str = r#"<style>
:root { color-scheme: light dark; }
body { font-family: system-ui, -apple-system, "Segoe UI", sans-serif; margin: 2rem auto; max-width: 1100px; padding: 0 1rem; line-height: 1.5; }
h1 { font-size: 1.5rem; margin-bottom: 0.2rem; }
h2 { font-size: 1.1rem; margin-top: 2rem; border-bottom: 1px solid #8884; padding-bottom: 0.2rem; }
.meta { color: #777; font-size: 0.9rem; }
.cards { display: flex; flex-wrap: wrap; gap: 1rem; margin: 1rem 0; }
.card { border: 1px solid #8884; border-radius: 8px; padding: 0.6rem 1rem; min-width: 10rem; }
.card .label { font-size: 0.8rem; color: #777; }
.card .value { font-size: 1.2rem; font-weight: 600; }
table { border-collapse: collapse; width: 100%; font-size: 0.92rem; }
th, td { text-align: left; padding: 0.35rem 0.6rem; border-bottom: 1px solid #8883; vertical-align: top; }
th { cursor: pointer; user-select: none; white-space: nowrap; }
td.num, th.num { text-align: right; white-space: nowrap; }
code { word-break: break-all; }
.bar { display: inline-block; width: 6rem; height: 0.6rem; background: #8883; border-radius: 3px; overflow: hidden; margin-left: 0.4rem; vertical-align: middle; }
.bar > span { display: block; height: 100%; background: #4a9; }
pre { overflow-x: auto; font-size: 0.85rem; }
details { margin-top: 1rem; }
.warn { color: #b80; }
</style>"#;

const SCRIPT: &str = r#"<script>
document.querySelectorAll('table.sortable th').forEach(function (th, index) {
  th.addEventListener('click', function () {
    var table = th.closest('table');
    var tbody = table.tBodies[0];
    var rows = Array.prototype.slice.call(tbody.rows);
    var dir = th.getAttribute('data-dir') === 'desc' ? 1 : -1;
    th.setAttribute('data-dir', dir === 1 ? 'desc' : 'asc');
    rows.sort(function (a, b) {
      var ac = a.cells[index], bc = b.cells[index];
      var av = ac.getAttribute('data-value') || ac.textContent;
      var bv = bc.getAttribute('data-value') || bc.textContent;
      var an = Number(av), bn = Number(bv);
      if (!isNaN(an) && !isNaN(bn)) { return (an - bn) * dir; }
      return av.localeCompare(bv) * dir;
    });
    rows.forEach(function (row) { tbody.appendChild(row); });
  });
});
</script>"#;

pub(crate) fn write_html(
    w: &mut dyn Write,
    outcome: &ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
) -> io::Result<()> {
    let root = outcome.tree.node(outcome.tree.root);
    let total = opts.metric(root);
    let root_label = html_escape(&meta.root);

    writeln!(w, "<!DOCTYPE html>")?;
    writeln!(w, "<html lang=\"ja\"><head><meta charset=\"utf-8\">")?;
    writeln!(
        w,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
    )?;
    writeln!(
        w,
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'\">"
    )?;
    writeln!(w, "<title>dskr レポート - {root_label}</title>")?;
    w.write_all(STYLE.as_bytes())?;
    w.write_all(b"</head><body>")?;

    writeln!(w, "<h1>ディスク使用量レポート</h1>")?;
    writeln!(w, "<div class=\"meta\"><code>{root_label}</code></div>")?;
    writeln!(
        w,
        "<div class=\"meta\">生成: {} / {} / {} スレッド / {:.2} 秒</div>",
        html_escape(&meta.generated_at),
        root_kind_display(meta.root_kind),
        meta.threads,
        meta.elapsed_ms as f64 / 1000.0
    )?;
    if meta.cancelled {
        writeln!(
            w,
            "<p class=\"warn\">注意: 走査は中止されました (途中までの結果)</p>"
        )?;
    }
    if meta.truncated {
        writeln!(
            w,
            "<p class=\"warn\">注意: 深さ/件数の上限により一部が省略されています</p>"
        )?;
    }
    if meta.errors > 0 {
        writeln!(
            w,
            "<p class=\"warn\">注意: アクセスできない項目があります。合計は不完全です</p>"
        )?;
    }
    let blocked = outcome.tree.blocked_count();
    if blocked > 0 {
        writeln!(
            w,
            "<p class=\"warn\">注意: -x / --local-only により対象外のフォルダが {} 件あります (中身は集計されません)</p>",
            count(blocked)
        )?;
    }

    writeln!(w, "<div class=\"cards\">")?;
    card(w, "合計", &human_bytes(opts, total))?;
    card(w, "見かけのサイズ", &human_bytes(opts, root.total_logical))?;
    card(w, "ファイル数", &count(root.file_count))?;
    card(w, "フォルダ数", &count(root.dir_count))?;
    card(w, "エラー数", &count(meta.errors))?;
    if blocked > 0 {
        card(w, "対象外フォルダ", &count(blocked))?;
    }
    writeln!(w, "</div>")?;

    let dirs = collect_top(
        outcome,
        meta,
        EntryFilter::Dirs,
        opts.top,
        opts.metric_alloc,
    );
    writeln!(w, "<h2>容量の大きいフォルダ ({})</h2>", dirs.len())?;
    if dirs.is_empty() {
        writeln!(w, "<p>(なし)</p>")?;
    } else {
        writeln!(w, "<table class=\"sortable\"><thead><tr>")?;
        writeln!(w, "<th class=\"num\">割合</th><th class=\"num\">サイズ</th><th class=\"num\">ファイル数</th><th>パス</th>")?;
        writeln!(w, "</tr></thead><tbody>")?;
        for e in &dirs {
            let p = percent(e.size, total);
            writeln!(w, "<tr>")?;
            writeln!(
                w,
                "<td class=\"num\" data-value=\"{p:.4}\">{p:.1}%<span class=\"bar\"><span style=\"width:{p:.1}%\"></span></span></td>",
            )?;
            writeln!(
                w,
                "<td class=\"num\" data-value=\"{}\">{}</td>",
                e.size,
                human_bytes(opts, e.size)
            )?;
            writeln!(
                w,
                "<td class=\"num\" data-value=\"{}\">{}</td>",
                e.files,
                count(e.files)
            )?;
            writeln!(w, "<td><code>{}</code></td>", html_escape(&e.path))?;
            writeln!(w, "</tr>")?;
        }
        writeln!(w, "</tbody></table>")?;
    }

    let files = collect_top(
        outcome,
        meta,
        EntryFilter::Files,
        opts.top,
        opts.metric_alloc,
    );
    writeln!(w, "<h2>容量の大きいファイル ({})</h2>", files.len())?;
    if files.is_empty() {
        writeln!(w, "<p>(なし)</p>")?;
    } else {
        writeln!(w, "<table class=\"sortable\"><thead><tr>")?;
        writeln!(
            w,
            "<th class=\"num\">割合</th><th class=\"num\">サイズ</th><th>パス</th>"
        )?;
        writeln!(w, "</tr></thead><tbody>")?;
        for e in &files {
            let p = percent(e.size, total);
            writeln!(w, "<tr>")?;
            writeln!(w, "<td class=\"num\" data-value=\"{p:.4}\">{p:.1}%</td>")?;
            writeln!(
                w,
                "<td class=\"num\" data-value=\"{}\">{}</td>",
                e.size,
                human_bytes(opts, e.size)
            )?;
            writeln!(w, "<td><code>{}</code></td>", html_escape(&e.path))?;
            writeln!(w, "</tr>")?;
        }
        writeln!(w, "</tbody></table>")?;
    }

    if !outcome.errors.is_empty() {
        writeln!(w, "<h2>エラー</h2>")?;
        if (outcome.errors.len() as u64) < meta.errors {
            writeln!(
                w,
                "<p class=\"warn\">全 {} 件のうち先頭 {} 件を表示しています</p>",
                meta.errors,
                outcome.errors.len()
            )?;
        }
        writeln!(
            w,
            "<table><thead><tr><th>パス</th><th>内容</th></tr></thead><tbody>"
        )?;
        for e in &outcome.errors {
            writeln!(
                w,
                "<tr><td><code>{}</code></td><td>{}</td></tr>",
                html_escape(&e.path),
                html_escape(&e.message)
            )?;
        }
        writeln!(w, "</tbody></table>")?;
    }

    if opts.depth > 0 {
        let tree = render_tree(outcome, meta, opts, false);
        writeln!(
            w,
            "<details><summary>フォルダ構成 (深さ {})</summary><pre>{}</pre></details>",
            opts.depth,
            html_escape_pre(tree.trim_end())
        )?;
    }

    w.write_all(SCRIPT.as_bytes())?;
    w.write_all(b"</body></html>\n")?;
    Ok(())
}

fn card(w: &mut dyn Write, label: &str, value: &str) -> io::Result<()> {
    writeln!(
        w,
        "<div class=\"card\"><div class=\"label\">{}</div><div class=\"value\">{}</div></div>",
        html_escape(label),
        html_escape(value)
    )
}
