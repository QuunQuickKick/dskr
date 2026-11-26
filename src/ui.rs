use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, ExportFocus, Mode, EXPORT_FORMATS};
use crate::format::{human_count, pct, sanitize_display};
use crate::path::short_path_display;
use crate::scan::node::{NodeKind, Tree};
use crate::sort::SortField;
use crate::timefmt;

const SELECTED_BG: Color = Color::Rgb(45, 50, 70);

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // ヘルプは全画面表示にして、下のフッターと重ならないようにする。
    if app.mode == Mode::Help {
        draw_help(f, app, area);
        return;
    }

    let header_len: u16 = if app.scanning { 3 } else { 2 };
    let footer_len: u16 = 2 + u16::from(app.message.is_some());
    let chunks = Layout::vertical([
        Constraint::Length(header_len),
        Constraint::Min(3),
        Constraint::Length(footer_len),
    ])
    .split(area);

    draw_header(f, app, chunks[0]);
    if app.mode == Mode::Errors {
        draw_errors(f, app, chunks[1]);
    } else {
        draw_body(f, app, chunks[1]);
    }
    draw_footer(f, app, chunks[2]);

    if app.mode == Mode::ConfirmQuit {
        draw_confirm_quit(f, area);
    } else if app.mode == Mode::ExportForm {
        draw_export_form(f, app, area);
    } else if app.mode == Mode::ConfirmDelete {
        draw_confirm_delete(f, app, area);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let title = format!(
        "dskr v{}  {}  [{}]  スレッド {}{}",
        env!("CARGO_PKG_VERSION"),
        sanitize_display(&app.root_display),
        app.root_kind.display_label(),
        app.threads,
        if app.allow_delete {
            "  削除:有効"
        } else {
            ""
        },
    );
    lines.push(Line::from(Span::styled(
        fit_width(&title, area.width as usize),
        Style::new().bold(),
    )));

    if app.scanning {
        let p = &app.progress;
        let secs = p.elapsed().as_secs_f64().max(0.001);
        let entries = p.entries.load(std::sync::atomic::Ordering::Relaxed);
        let rate = entries as f64 / secs;
        let status = if app.paused {
            "一時停止"
        } else if app.cancel_requested {
            "中止要求済み"
        } else {
            "走査中"
        };
        let line = format!(
            "{}  {} 項目  {} / {}  {} 項目/秒  {}  ({}秒)",
            status,
            human_count(entries),
            app.size_format
                .format(p.bytes_alloc.load(std::sync::atomic::Ordering::Relaxed)),
            app.size_format
                .format(p.bytes_logical.load(std::sync::atomic::Ordering::Relaxed)),
            human_count(rate as u64),
            short_path_display(
                &sanitize_display(&p.current_dir()),
                area.width.saturating_sub(30) as usize
            ),
            p.elapsed().as_secs(),
        );
        lines.push(Line::from(fit_width(&line, area.width as usize)));
        lines.push(Line::from(Span::styled(
            fit_width(
                "Space: 一時停止/再開   Esc: 中止 (途中までの結果を表示)   q: 中止して終了",
                area.width as usize,
            ),
            Style::new().fg(Color::DarkGray),
        )));
    } else if let Some(out) = app.outcome.as_ref() {
        let n = out.tree.node(app.cur());
        let metric = if app.show_allocated {
            n.total_alloc
        } else {
            n.total_logical
        };
        let errs = if out.error_count > 0 {
            format!("  エラー {} (E:一覧)", human_count(out.error_count))
        } else {
            String::new()
        };
        let line = format!(
            "合計 {} ({})  ファイル {}  フォルダ {}  単位 {} / {}{}  {:.1}秒  並び順 {}",
            app.size_format.format(metric),
            app.metric_display_label(),
            human_count(n.file_count),
            human_count(n.dir_count),
            app.size_format.display_label(),
            app.size_format.system_label(),
            errs,
            out.elapsed.as_secs_f64(),
            app.sort_label(),
        );
        lines.push(Line::from(fit_width(&line, area.width as usize)));
    } else {
        lines.push(Line::from("走査結果がありません"));
    }

    f.render_widget(Paragraph::new(lines), area);
}

/// 表示列の幅。端末幅に応じてバーや割合・更新日時を省略する。
#[derive(Clone, Copy, Debug)]
struct Columns {
    size: usize,
    pct: usize,
    bar: usize,
    name: usize,
    count: usize,
    mtime: usize,
}

/// 更新日時 `YYYY-MM-DD HH:MM` の表示幅。
const MTIME_W: usize = 16;
/// 更新日時列を出す最小の内部幅。
const MTIME_MIN_INNER: usize = 100;

impl Columns {
    fn new(inner_w: usize) -> Self {
        let size = if inner_w >= 40 { 12 } else { 10 };
        let count = if inner_w >= 40 { 12 } else { 7 };
        let pct = if inner_w >= 34 { 6 } else { 0 };
        let mtime = if inner_w >= MTIME_MIN_INNER {
            MTIME_W
        } else {
            0
        };

        let base = size
            + 2
            + pct
            + usize::from(pct > 0)
            + count
            + 1
            + if mtime > 0 { mtime + 1 } else { 0 };

        let mut bar = 0usize;
        if inner_w >= 72 {
            let available = inner_w.saturating_sub(base + 8);
            let candidate = (inner_w / 6).clamp(8, 20).min(available);
            if candidate >= 8 {
                bar = candidate;
            }
        }
        let bar_gap = if bar > 0 { 2 } else { 0 };
        let fixed = base + bar + bar_gap;
        let name = inner_w.saturating_sub(fixed).max(4);
        Columns {
            size,
            pct,
            bar,
            name,
            count,
            mtime,
        }
    }
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;

    let Some(tree) = app.tree() else {
        let msg = if app.scanning {
            "走査中… (進み具合は上部を参照)  Space: 一時停止  Esc: 中止  q: 終了"
        } else {
            "走査結果がありません (走査に失敗しました)"
        };
        f.render_widget(
            Paragraph::new(msg).block(Block::new().borders(Borders::ALL).title(" dskr ")),
            area,
        );
        return;
    };

    let cur = app.cur();
    let total = if app.show_allocated {
        tree.node(cur).total_alloc
    } else {
        tree.node(cur).total_logical
    };
    let cols = Columns::new(inner_w);

    let title = {
        let mut t = format!(" {}", sanitize_display(&app.current_path()));
        if !app.filter.is_empty() {
            t.push_str(&format!(
                "  [絞込: {} ({}/{})]",
                sanitize_display(&app.filter),
                app.listing.len(),
                tree.node(cur).children.len()
            ));
        }
        t.push(' ');
        fit_width(&t, area.width.saturating_sub(2) as usize)
    };
    let block = Block::new().borders(Borders::ALL).title(title);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    f.render_widget(block, area);

    let header_rows = if inner_h >= 3 {
        2
    } else if inner_h >= 1 {
        1
    } else {
        0
    };
    let rows_h = inner_h.saturating_sub(header_rows);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(inner_h);
    if header_rows >= 1 {
        lines.push(header_line(app, &cols));
    }
    if header_rows >= 2 {
        lines.push(Line::from(Span::styled(
            "─".repeat(inner_w),
            Style::new().fg(Color::DarkGray),
        )));
    }

    if app.listing.is_empty() {
        let msg = if tree.node(cur).truncated {
            " (子要素は上限により省略されています)"
        } else {
            " (子要素なし)"
        };
        lines.push(Line::from(Span::styled(
            msg.to_string(),
            Style::new().fg(Color::DarkGray),
        )));
    } else {
        let len = app.listing.len();
        let start = if rows_h == 0 {
            0
        } else {
            app.sel
                .saturating_sub(rows_h / 2)
                .min(len.saturating_sub(rows_h.min(len)))
        };
        let end = (start + rows_h).min(len);
        for (vis, &idx) in app.listing[start..end].iter().enumerate() {
            let selected = start + vis == app.sel;
            lines.push(row(app, tree, idx, selected, total, &cols));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn header_line(app: &App, cols: &Columns) -> Line<'static> {
    let style = Style::new()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let sort_field = app.sort_field;
    let arrow = app.sort_dir.arrow();

    let size_label = if sort_field == SortField::Size {
        format!("サイズ{arrow}")
    } else {
        "サイズ".to_string()
    };
    spans.push(Span::styled(pad_left(&size_label, cols.size), style));
    spans.push(Span::styled("  ", style));

    let ratio_w =
        cols.pct + usize::from(cols.pct > 0) + cols.bar + if cols.bar > 0 { 2 } else { 0 };
    if ratio_w > 0 {
        spans.push(Span::styled(pad_right("割合", ratio_w), style));
    }

    let name_label = if sort_field == SortField::Name {
        format!("名前{arrow}")
    } else {
        "名前".to_string()
    };
    spans.push(Span::styled(pad_right(&name_label, cols.name), style));

    let count_label = if sort_field == SortField::Count {
        format!("ファイル数{arrow}")
    } else {
        "ファイル数".to_string()
    };
    spans.push(Span::styled(" ", style));
    spans.push(Span::styled(pad_left(&count_label, cols.count), style));

    if cols.mtime > 0 {
        let mtime_label = if sort_field == SortField::Mtime {
            format!("更新日時{arrow}")
        } else {
            "更新日時".to_string()
        };
        spans.push(Span::styled(" ", style));
        spans.push(Span::styled(pad_right(&mtime_label, cols.mtime), style));
    }

    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn row(
    app: &App,
    tree: &Tree,
    idx: u32,
    selected: bool,
    total: u64,
    cols: &Columns,
) -> Line<'static> {
    let n = tree.node(idx);
    let metric = if app.show_allocated {
        n.total_alloc
    } else {
        n.total_logical
    };
    let p = pct(metric, total);

    let base = if selected {
        Style::new().bg(SELECTED_BG)
    } else {
        Style::new()
    };
    let dim = base.fg(Color::DarkGray);

    let size_text = pad_left(&app.size_format.format(metric), cols.size);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(10);
    spans.push(Span::styled(size_text, base));
    spans.push(Span::styled("  ", base));

    if cols.pct > 0 {
        let pct_text = format!("{p:>width$.1}%", width = cols.pct.saturating_sub(1));
        spans.push(Span::styled(pad_left(&pct_text, cols.pct), dim));
        spans.push(Span::styled(" ", base));
    }

    if cols.bar > 0 {
        let filled = ((p / 100.0) * cols.bar as f64).round() as usize;
        let filled = filled.min(cols.bar);
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(cols.bar - filled));
        spans.push(Span::styled(
            bar,
            if filled > 0 {
                base.fg(Color::Cyan)
            } else {
                dim
            },
        ));
        spans.push(Span::styled("  ", base));
    }

    let mut name = sanitize_display(&n.name.to_string_lossy());
    match n.kind {
        NodeKind::Dir => name.push('/'),
        NodeKind::Other => name.push_str(" (link)"),
        NodeKind::Blocked => name.push_str(" (対象外)"),
        NodeKind::File => {}
    }

    let marker_raw = if n.err_count > 0 {
        format!(" !{}", n.err_count)
    } else {
        String::new()
    };
    let marker = fit_width(&marker_raw, cols.name);
    let marker_w = marker.width();
    let name_avail = cols.name.saturating_sub(marker_w);
    let name = fit_width(&name, name_avail);
    let name_pad = cols.name.saturating_sub(name.width() + marker_w);

    let name_style = match n.kind {
        NodeKind::Dir => base.fg(Color::Cyan).add_modifier(Modifier::BOLD),
        NodeKind::Other | NodeKind::Blocked => dim,
        NodeKind::File => base,
    };
    spans.push(Span::styled(name, name_style));
    if !marker.is_empty() {
        spans.push(Span::styled(marker, base.fg(Color::Red)));
    }
    if name_pad > 0 {
        spans.push(Span::styled(" ".repeat(name_pad), base));
    }

    let count_text = match n.kind {
        NodeKind::Dir => human_count(n.file_count),
        _ => "-".to_string(),
    };
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(pad_left(&count_text, cols.count), dim));

    if cols.mtime > 0 {
        let mtime_text = if n.mtime > 0 {
            timefmt::local_datetime(n.mtime)
        } else {
            "-".to_string()
        };
        spans.push(Span::styled(" ", base));
        spans.push(Span::styled(pad_right(&mtime_text, cols.mtime), dim));
    }

    Line::from(spans)
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let mut lines = Vec::with_capacity(3);

    lines.push(Line::from(Span::styled(
        short_path_display(&sanitize_display(&app.current_path()), width),
        Style::new().fg(Color::Cyan),
    )));

    lines.push(Line::from(Span::styled(
        fit_width(&footer_hints(app, width), width),
        Style::new().fg(Color::DarkGray),
    )));

    if let Some(msg) = &app.message {
        lines.push(Line::from(Span::styled(
            fit_width(&format!(" {}", sanitize_display(msg)), width),
            Style::new().fg(Color::Yellow),
        )));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn footer_hints(app: &App, width: usize) -> String {
    if app.scanning {
        return if app.paused {
            "Space: 再開   Esc: 中止 (部分結果を表示)   q: 中止して終了".to_string()
        } else {
            "Space: 一時停止   Esc: 中止 (部分結果を表示)   q: 中止して終了".to_string()
        };
    }

    let sort = app.sort_label();
    let delete = if app.allow_delete { "[d]削除 " } else { "" };
    if width >= 110 {
        format!(
            "[Enter]開く [h/BS]上へ [j/k]移動 [s]並び順:{} [S]逆順 [u]単位:{} [U]IEC/SI [/]絞込 [E]エラー [e]保存 {delete}[?]ヘルプ [q]終了",
            sort,
            app.size_format.display_label()
        )
    } else if width >= 80 {
        format!(
            "[Enter]開く [h/BS]上へ [s]{} [S]逆順 [u]単位 [U]IEC/SI [/]絞込 {delete}[?]ヘルプ [q]終了",
            sort
        )
    } else {
        "[Enter]開く [h/BS]上へ [s]並び順 [S]逆順 [/]絞込 [?]ヘルプ [q]終了".to_string()
    }
}

fn draw_errors(f: &mut Frame, app: &App, area: Rect) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let (total, recorded) = app
        .outcome
        .as_ref()
        .map(|o| (o.error_count, o.errors.len()))
        .unwrap_or((0, 0));
    let title = fit_width(
        &format!(
            " エラー: {} 件 ({} 件を表示)  [Esc] 閉じる ",
            total, recorded
        ),
        area.width.saturating_sub(2) as usize,
    );
    let block = Block::new().borders(Borders::ALL).title(title);

    let Some(out) = app.outcome.as_ref() else {
        f.render_widget(Paragraph::new("エラーはありません").block(block), area);
        return;
    };
    if out.errors.is_empty() {
        f.render_widget(Paragraph::new("エラーはありません").block(block), area);
        return;
    }
    let start = app.err_sel.saturating_sub(inner_h / 2).min(
        out.errors
            .len()
            .saturating_sub(inner_h.min(out.errors.len())),
    );
    let end = (start + inner_h).min(out.errors.len());
    let mut lines = Vec::new();
    for (i, e) in out.errors[start..end].iter().enumerate() {
        let sel = start + i == app.err_sel;
        let style = if sel {
            Style::new().bg(SELECTED_BG)
        } else {
            Style::new()
        };
        let text = format!(
            "{}: {}",
            short_path_display(&sanitize_display(&e.path), inner_w / 2),
            sanitize_display(&e.message)
        );
        lines.push(Line::from(vec![
            Span::styled(format!("{:>5} ", start + i + 1), style.fg(Color::DarkGray)),
            Span::styled(fit_width(&text, inner_w.saturating_sub(6)), style),
        ]));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn help_lines() -> Vec<&'static str> {
    vec![
        "キー操作",
        "",
        "  j/k / ↑↓      項目を移動",
        "  PgUp/PgDn      ページ移動 (ヘルプ)",
        "  Enter / l      フォルダを開く",
        "  h / BS         一つ上へ (左キーも可)",
        "  g / G          先頭 / 末尾へ",
        "",
        "  s              並び順 (サイズ→名前→ファイル数→更新日時)",
        "  S              昇順 / 降順の反転",
        "  a              合計の種類 (ディスク使用量 ⇔ 見かけのサイズ)",
        "  u              表示単位 (自動→B→KiB→MiB→...)",
        "  U              単位の基準 (IEC=1024基準 ⇔ SI=1000基準)",
        "  /              名前で絞り込み (Enter 適用 / Esc 解除)",
        "  E              エラー一覧",
        "  e              結果を保存 (形式・件数・深さを指定)",
        "  d              選択項目を削除 (--allow-delete 指定時のみ)",
        "  ?              このヘルプ",
        "  q              終了",
        "",
        "  削除は確認ダイアログで確定します (ファイルは y、フォルダは yes 入力)",
        "  削除は完全削除で、フォルダは中身ごと削除されます",
        "  削除できるのは現在のフォルダの子項目だけです (走査中は不可)",
        "  走査中: Space=一時停止/再開, Esc=中止 (途中までの結果を表示), q=中止して終了",
        "  既定の並び順: サイズの大きい順",
        "  隠しファイル・システムファイルは既定で非表示です (--hidden で表示)",
        "  Esc で中止しても q → y で強制終了できます",
        "  更新日時は画面が広いときだけ表示されます",
        "  保存設定: Tab で移動, ←→ で形式, ↑↓ で ±1, Enter で保存",
    ]
}

fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
    f.render_widget(Clear, area);
    let lines: Vec<Line<'static>> = help_lines()
        .into_iter()
        .map(|text| Line::from(fit_width(text, area.width.saturating_sub(4) as usize)))
        .collect();

    let inner_h = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_h);
    app.help_max_scroll = max_scroll.min(u16::MAX as usize) as u16;
    let scroll = (app.help_scroll as usize).min(max_scroll);
    app.help_scroll = scroll as u16;

    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .title(" ヘルプ (?/Esc/q で閉じる) "),
            )
            .scroll((scroll as u16, 0)),
        area,
    );
}

fn draw_confirm_quit(f: &mut Frame, area: Rect) {
    let rect = centered(area, 62, 5);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(" 走査を中止して終了しますか?"),
            Line::from(""),
            Line::from(" [y] 終了   [n/Esc] キャンセル"),
        ])
        .block(Block::new().borders(Borders::ALL).title(" 確認 ")),
        rect,
    );
}

fn draw_confirm_delete(f: &mut Frame, app: &App, area: Rect) {
    let rect = centered(area, 76, 18);
    f.render_widget(Clear, rect);
    let inner_w = rect.width.saturating_sub(2) as usize;
    let Some(target) = app.delete_target.as_ref() else {
        f.render_widget(
            Paragraph::new("削除対象がありません")
                .block(Block::new().borders(Borders::ALL).title(" 削除の確認 ")),
            rect,
        );
        return;
    };

    let dim = Style::new().fg(Color::DarkGray);
    let value = Style::new().fg(Color::Cyan);
    let kind = match target.kind {
        NodeKind::Dir => "フォルダ (中身ごと)",
        NodeKind::File => "ファイル",
        NodeKind::Other => "リンク / 特殊 (中身は削除しません)",
        NodeKind::Blocked => "走査対象外 (中身は削除しません)",
    };
    let path = target.path.display().to_string();
    let path = short_path_display(
        &sanitize_display(&path),
        inner_w.saturating_sub(" 対象   : ".len()),
    );

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" 対象   : ", dim),
        Span::styled(fit_width(&path, inner_w.saturating_sub(9)), value),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" 種類   : ", dim),
        Span::styled(kind, Style::new()),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" サイズ : ", dim),
        Span::styled(
            format!(
                "{} ({})",
                app.size_format.format(target.bytes),
                app.metric_display_label()
            ),
            Style::new(),
        ),
    ]));
    if target.kind == NodeKind::Dir {
        lines.push(Line::from(vec![
            Span::styled(" 内容   : ", dim),
            Span::styled(
                format!(
                    "ファイル {} / フォルダ {}",
                    human_count(target.files),
                    human_count(target.dirs)
                ),
                Style::new(),
            ),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        fit_width(" この操作は取り消せません。", inner_w),
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    if target.kind == NodeKind::Dir {
        lines.push(Line::from(Span::styled(
            fit_width(" フォルダの中身もすべて削除されます。", inner_w),
            Style::new().fg(Color::Red),
        )));
        if target.truncated {
            lines.push(Line::from(Span::styled(
                fit_width(
                    " 表示・集計されていない項目も含まれる可能性があります。",
                    inner_w,
                ),
                Style::new().fg(Color::Red),
            )));
        }
    } else if target.kind == NodeKind::Other {
        lines.push(Line::from(Span::styled(
            fit_width(" リンク先は削除せず、リンク自体だけを削除します。", inner_w),
            Style::new(),
        )));
    } else if target.kind == NodeKind::Blocked {
        lines.push(Line::from(Span::styled(
            fit_width(
                " 走査していない項目です。空のフォルダしか削除できません。",
                inner_w,
            ),
            Style::new().fg(Color::Yellow),
        )));
    }
    if app.profile_label == "network" {
        lines.push(Line::from(Span::styled(
            fit_width(" ネットワーク上の項目です。", inner_w),
            Style::new().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));

    if target.kind == NodeKind::Dir {
        lines.push(Line::from(vec![
            Span::styled(" 確認のため yes と入力: ", Style::new()),
            Span::styled(
                format!("[ {} ]", target.input),
                Style::new().bg(SELECTED_BG).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            fit_width(" [Enter] 削除   [Esc] キャンセル", inner_w),
            dim,
        )));
    } else {
        lines.push(Line::from(Span::styled(
            fit_width(" [y] 削除   [Esc] キャンセル", inner_w),
            Style::new().fg(Color::Yellow),
        )));
    }
    lines.push(match &target.error {
        Some(err) => Line::from(Span::styled(
            fit_width(&format!(" ! {err}"), inner_w),
            Style::new().fg(Color::Red),
        )),
        None => Line::from(""),
    });

    f.render_widget(
        Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title(" 削除の確認 ")),
        rect,
    );
}

fn draw_export_form(f: &mut Frame, app: &App, area: Rect) {
    let rect = centered(area, 72, 13);
    f.render_widget(Clear, rect);
    let inner_w = rect.width.saturating_sub(2) as usize;
    let format = EXPORT_FORMATS[app.export_sel.min(EXPORT_FORMATS.len() - 1)];
    let focus = app.export_focus;
    let marker = |f: ExportFocus| if focus == f { ">" } else { " " };
    let field_style = |f: ExportFocus| {
        if focus == f {
            Style::new().bg(SELECTED_BG).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        }
    };
    let dim = Style::new().fg(Color::DarkGray);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} 形式   : ", marker(ExportFocus::Format)),
            Style::new(),
        ),
        Span::styled(
            pad_right(&format!("{}  .{}", format.label(), format.extension()), 28),
            field_style(ExportFocus::Format),
        ),
    ]));

    let top_enabled = format.uses_top();
    let top_text = if top_enabled {
        format!("[ {} ]", app.export_top)
    } else {
        "—".to_string()
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} 件数   : ", marker(ExportFocus::Top)),
            Style::new(),
        ),
        Span::styled(
            pad_right(&top_text, 28),
            if top_enabled {
                field_style(ExportFocus::Top)
            } else {
                dim
            },
        ),
        Span::styled(" (0 = 無制限)", dim),
    ]));

    let depth_enabled = format.uses_depth();
    let depth_text = if depth_enabled {
        format!("[ {} ]", app.export_depth)
    } else {
        "—".to_string()
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} 深さ   : ", marker(ExportFocus::Depth)),
            Style::new(),
        ),
        Span::styled(
            pad_right(&depth_text, 28),
            if depth_enabled {
                field_style(ExportFocus::Depth)
            } else {
                dim
            },
        ),
        Span::styled(" (0 = ツリーなし)", dim),
    ]));

    lines.push(Line::from(vec![
        Span::styled(format!(" {} ", marker(ExportFocus::Save)), Style::new()),
        Span::styled(
            if focus == ExportFocus::Save {
                "[ 保存 ]"
            } else {
                "  保存  "
            },
            field_style(ExportFocus::Save),
        ),
    ]));

    lines.push(Line::from(""));
    let preview = app.export_preview_path().display().to_string();
    let path = short_path_display(&sanitize_display(&preview), inner_w.saturating_sub(9));
    lines.push(Line::from(Span::styled(
        fit_width(&format!(" 保存先: {path}"), inner_w),
        Style::new().fg(Color::Cyan),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        fit_width(" [Tab] 項目移動  [←/→] 形式変更  [↑/↓] ±1", inner_w),
        dim,
    )));
    lines.push(Line::from(Span::styled(
        fit_width(" [Enter] 次へ / 保存   [Esc] キャンセル", inner_w),
        dim,
    )));
    lines.push(match &app.export_error {
        Some(err) => Line::from(Span::styled(
            fit_width(&format!(" ! {err}"), inner_w),
            Style::new().fg(Color::Red),
        )),
        None => Line::from(""),
    });

    f.render_widget(
        Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title(" 保存の設定 ")),
        rect,
    );
}

/// 表示幅 (日本語は2桁) で末尾を省略する (先頭を残す)。
fn fit_width(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

fn pad_left(s: &str, width: usize) -> String {
    let t = fit_width(s, width);
    let pad = width.saturating_sub(t.width());
    format!("{}{}", " ".repeat(pad), t)
}

fn pad_right(s: &str, width: usize) -> String {
    let t = fit_width(s, width);
    let pad = width.saturating_sub(t.width());
    format!("{}{}", t, " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DeleteTarget;
    use crate::path::RootKind;
    use crate::scan::engine::{Control, Progress};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    fn test_app() -> App {
        App::new(
            std::path::PathBuf::from("/tmp"),
            RootKind::Local,
            "local".to_string(),
            true,
            false,
            Arc::new(Progress::new()),
            Arc::new(Control::new()),
            2,
            "{}".to_string(),
        )
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn padding_handles_wide_chars() {
        assert_eq!(pad_left("あ", 4).width(), 4);
        assert_eq!(pad_right("あ", 4).width(), 4);
        assert_eq!(fit_width("abcdef", 4), "abc…");
        assert!(fit_width("あいうえお", 6).width() <= 6);
    }

    #[test]
    fn columns_adapt_to_width() {
        let wide = Columns::new(120);
        assert!(wide.bar >= 8);
        assert!(wide.name > 20);
        assert_eq!(wide.mtime, MTIME_W);
        let medium = Columns::new(80);
        assert_eq!(medium.mtime, 0);
        assert!(medium.name > 0);
        let narrow = Columns::new(60);
        assert_eq!(narrow.bar, 0);
        assert_eq!(narrow.mtime, 0);
        assert!(narrow.name > 0);
    }

    #[test]
    fn help_renders_full_screen_without_panic() {
        let mut app = test_app();
        app.scanning = false;
        app.mode = Mode::Help;
        for (w, h) in [(80, 24), (40, 12), (20, 8)] {
            let text = render(&mut app, w, h);
            let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(compact.contains("キー操作"));
        }
        assert!(help_lines().iter().any(|l| l.contains("削除")));
    }

    #[test]
    fn delete_dialog_renders_warning_and_input() {
        let mut app = test_app();
        app.allow_delete = true;
        app.scanning = false;
        app.mode = Mode::ConfirmDelete;
        app.delete_target = Some(DeleteTarget {
            node: 1,
            path: std::path::PathBuf::from("/tmp/victim"),
            kind: NodeKind::Dir,
            bytes: 1024,
            files: 3,
            dirs: 1,
            truncated: true,
            input: "ye".to_string(),
            error: Some("確認のため yes と入力してください".to_string()),
        });

        let text = render(&mut app, 100, 30);
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("削除の確認"), "{text}");
        assert!(compact.contains("victim"), "{text}");
        assert!(compact.contains("取り消せません"), "{text}");
        assert!(compact.contains("中身もすべて削除"), "{text}");
        assert!(compact.contains("集計されていない"), "{text}");
        assert!(compact.contains("yes"), "{text}");
    }

    #[test]
    fn delete_hint_and_enabled_marker_follow_option() {
        let mut app = test_app();
        app.scanning = false;

        let disabled = render(&mut app, 120, 20);
        let disabled: String = disabled.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!disabled.contains("[d]削除"), "{disabled}");
        assert!(!disabled.contains("削除:有効"), "{disabled}");

        app.allow_delete = true;
        let enabled = render(&mut app, 120, 20);
        let enabled: String = enabled.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(enabled.contains("[d]削除"), "{enabled}");
        assert!(enabled.contains("削除:有効"), "{enabled}");
    }

    #[test]
    fn footer_uses_bs_label_and_fits_width() {
        let mut app = test_app();
        app.scanning = false;
        app.message = Some("x".repeat(300));
        let text = render(&mut app, 60, 20);
        assert!(text.contains("BS"));
        assert!(!text.contains("⌫"));
    }

    #[test]
    fn count_column_is_aligned_regardless_of_name_width() {
        use crate::scan::node::{Node, Tree};
        let mut tree = Tree::with_root("root".into(), 0);
        let short = tree
            .push_child_capped(0, Node::dir("a".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(short, Node::file("x".into(), short, 2, 10, 10, 0), 100)
            .unwrap();
        let long = tree
            .push_child_capped(
                0,
                Node::dir("a-very-long-directory-name".into(), 0, 1, 0),
                100,
            )
            .unwrap();
        tree.push_child_capped(long, Node::file("y".into(), long, 2, 20, 20, 0), 100)
            .unwrap();
        tree.aggregate();

        let app = test_app();
        let total = tree.node(tree.root).total_alloc;
        let cols = Columns::new(80);
        let line_a = row(&app, &tree, short, false, total, &cols);
        let line_b = row(&app, &tree, long, false, total, &cols);
        let start = |line: &Line| -> usize {
            line.spans[..line.spans.len() - 1]
                .iter()
                .map(|s| s.content.width())
                .sum()
        };
        assert_eq!(start(&line_a), start(&line_b));

        let header: String = header_line(&app, &cols)
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(header.contains("サイズ"));
        assert!(header.contains("割合"));
        assert!(header.contains("名前"));
        assert!(header.contains("ファイル数"));
    }

    #[test]
    fn default_sort_is_size_desc() {
        let app = test_app();
        assert_eq!(app.sort_field, SortField::Size);
        assert_eq!(app.sort_dir, crate::sort::SortDir::Desc);
    }

    #[test]
    fn sort_and_unit_keys() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = test_app();
        app.scanning = false;

        app.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE));
        assert_eq!(app.sort_dir, crate::sort::SortDir::Asc);
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(app.sort_field, SortField::Name);
        // 種類を変えても方向は維持
        assert_eq!(app.sort_dir, crate::sort::SortDir::Asc);

        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(
            app.size_format.display,
            crate::format::UnitDisplay::Fixed(0)
        );
        assert_eq!(app.size_format.system, crate::format::UnitSystem::Iec);
        app.on_key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::NONE));
        assert_eq!(app.size_format.system, crate::format::UnitSystem::Si);
        // 単位系を変えても表示単位は維持
        assert_eq!(
            app.size_format.display,
            crate::format::UnitDisplay::Fixed(0)
        );

        app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Help));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn body_renders_column_headers_and_largest_first() {
        use crate::scan::engine::ScanOutcome;
        use crate::scan::node::{Node, Tree};
        use std::time::Duration;

        let mut tree = Tree::with_root("/tmp".into(), 0);
        let big = tree
            .push_child_capped(0, Node::dir("big".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(big, Node::file("x".into(), big, 2, 300, 300, 0), 100)
            .unwrap();
        let small = tree
            .push_child_capped(0, Node::dir("small".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(small, Node::file("y".into(), small, 2, 10, 10, 0), 100)
            .unwrap();
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 4,
            dirs: 3,
            error_count: 0,
            bytes_alloc: 310,
            bytes_logical: 310,
            elapsed: Duration::from_millis(10),
            root: std::path::PathBuf::from("/tmp"),
        };

        let mut app = test_app();
        app.finish(outcome);
        let text = render(&mut app, 100, 24);
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("サイズ↓"));
        assert!(compact.contains("割合"));
        assert!(compact.contains("名前"));
        assert!(compact.contains("ファイル数"));
        // 既定はサイズ降順なので big が small より先
        let big_pos = text.find("big/").unwrap();
        let small_pos = text.find("small/").unwrap();
        assert!(big_pos < small_pos);
    }

    #[test]
    fn mtime_column_appears_only_on_wide_screens() {
        use crate::scan::engine::ScanOutcome;
        use crate::scan::node::{Node, Tree};
        use std::time::Duration;

        let mut tree = Tree::with_root("/tmp".into(), 1_704_067_200);
        tree.push_child_capped(
            0,
            Node::file("a.txt".into(), 0, 1, 10, 10, 1_704_067_200),
            100,
        )
        .unwrap();
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 2,
            dirs: 1,
            error_count: 0,
            bytes_alloc: 10,
            bytes_logical: 10,
            elapsed: Duration::from_millis(10),
            root: std::path::PathBuf::from("/tmp"),
        };

        let mut app = test_app();
        app.finish(outcome);

        let wide = render(&mut app, 130, 20);
        let wide_compact: String = wide.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(wide_compact.contains("更新日時"), "{wide}");

        let narrow = render(&mut app, 100, 20);
        let narrow_compact: String = narrow.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!narrow_compact.contains("更新日時"), "{narrow}");
    }

    #[test]
    fn export_form_renders_and_switches_format() {
        use crate::scan::engine::ScanOutcome;
        use crate::scan::node::{Node, Tree};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::time::Duration;

        let mut tree = Tree::with_root("/tmp".into(), 0);
        tree.push_child_capped(0, Node::file("a".into(), 0, 1, 1, 1, 0), 100)
            .unwrap();
        tree.aggregate();
        let outcome = ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 2,
            dirs: 1,
            error_count: 0,
            bytes_alloc: 1,
            bytes_logical: 1,
            elapsed: Duration::from_millis(1),
            root: std::path::PathBuf::from("/tmp"),
        };

        let mut app = test_app();
        app.finish(outcome);
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::ExportForm));
        assert_eq!(app.export_focus, ExportFocus::Format);

        let text = render(&mut app, 80, 24);
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(compact.contains("保存の設定"), "{text}");
        assert!(compact.contains("形式"), "{text}");
        assert!(compact.contains("件数"), "{text}");
        assert!(compact.contains("深さ"), "{text}");
        assert!(compact.contains("保存先"), "{text}");
        assert!(compact.contains("Tab"), "{text}");
        // JSON では top/depth が無効表示。
        assert!(compact.contains("—"), "{text}");

        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.current_export_format(),
            crate::output::OutputFormat::Markdown
        );
        assert_eq!(app.export_top, "10");
        assert_eq!(app.export_depth, "2");

        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Top);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Depth);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Save);

        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.message.as_deref().unwrap_or("").contains("キャンセル"));
    }

    #[test]
    fn export_selection_covers_all_formats() {
        let exts: Vec<&str> = EXPORT_FORMATS.iter().map(|f| f.extension()).collect();
        assert_eq!(exts, vec!["json", "md", "html", "csv", "txt", "txt", "txt"]);
    }
}
