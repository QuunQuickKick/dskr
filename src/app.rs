use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::delete;
use crate::format::SizeFormat;
use crate::output::{self, OutputFormat, OutputOptions};
use crate::path::RootKind;
use crate::report;
use crate::scan::engine::{Control, ControlState, Progress, ScanOutcome};
use crate::scan::node::{NodeKind, Tree};
use crate::scan::platform;
use crate::sort::{self, SortDir, SortField};

/// TUI の `e` で選べる保存形式。
pub const EXPORT_FORMATS: [OutputFormat; 7] = [
    OutputFormat::Json,
    OutputFormat::Markdown,
    OutputFormat::Html,
    OutputFormat::Csv,
    OutputFormat::Report,
    OutputFormat::Tree,
    OutputFormat::Flat,
];

/// 保存フォーム内のフォーカス位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFocus {
    Format,
    Top,
    Depth,
    Save,
}

/// 実際に書き出す内容。
pub struct ExportRequest {
    pub format: OutputFormat,
    pub top: usize,
    pub depth: usize,
    pub path: PathBuf,
}

/// 削除確認ダイアログの対象。
pub struct DeleteTarget {
    pub node: u32,
    pub path: PathBuf,
    pub kind: NodeKind,
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub truncated: bool,
    /// フォルダ削除時に「yes」と入力させるためのバッファ。
    pub input: String,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    Errors,
    ConfirmQuit,
    ExportForm,
    ConfirmDelete,
}

pub struct App {
    pub root: PathBuf,
    pub root_display: String,
    pub root_kind: RootKind,
    pub profile_label: String,
    pub show_allocated: bool,
    /// allocated (ディスク使用量) を実際に取得済みか。
    /// Windows のネットワークプロファイルでは取得しないことがある。
    pub alloc_collected: bool,
    pub allow_delete: bool,
    pub delete_target: Option<DeleteTarget>,
    pub outcome: Option<ScanOutcome>,
    pub stack: Vec<u32>,
    pub listing: Vec<u32>,
    pub sel: usize,
    pub sort_field: SortField,
    pub sort_dir: SortDir,
    pub size_format: SizeFormat,
    pub help_scroll: u16,
    pub help_max_scroll: u16,
    pub filter: String,
    pub mode: Mode,
    pub err_sel: usize,
    pub message: Option<String>,
    pub export_sel: usize,
    pub export_focus: ExportFocus,
    pub export_top: String,
    pub export_depth: String,
    pub export_error: Option<String>,
    pub export_dir: PathBuf,
    pub export_ts: String,
    pub scanning: bool,
    pub cancel_requested: bool,
    pub paused: bool,
    pub quit: bool,
    pub force_quit: bool,
    pub progress: Arc<Progress>,
    pub control: Arc<Control>,
    pub threads: usize,
    pub engine_options: String,
}

/// 削除対象のサブツリーに含まれるノード番号を集める。
fn subtree_indices(tree: &Tree, root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(idx) = stack.pop() {
        out.push(idx);
        stack.extend(tree.node(idx).children.iter().copied());
    }
    out
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: PathBuf,
        root_kind: RootKind,
        profile_label: String,
        show_allocated: bool,
        allow_delete: bool,
        progress: Arc<Progress>,
        control: Arc<Control>,
        threads: usize,
        engine_options: String,
    ) -> Self {
        let root_display = root.to_string_lossy().into_owned();
        // Windows ネットワークでは、明示的に --metric allocated を選んだ場合を
        // 除いて割当サイズを取得しない (サーバへの余計な往復を避ける)。
        let alloc_collected = show_allocated || !cfg!(windows) || profile_label != "network";
        App {
            root,
            root_display,
            root_kind,
            profile_label,
            show_allocated,
            alloc_collected,
            allow_delete,
            delete_target: None,
            outcome: None,
            stack: vec![0],
            listing: Vec::new(),
            sel: 0,
            sort_field: SortField::Size,
            sort_dir: SortDir::Desc,
            size_format: SizeFormat::default(),
            help_scroll: 0,
            help_max_scroll: 0,
            filter: String::new(),
            mode: Mode::Normal,
            err_sel: 0,
            message: None,
            export_sel: 0,
            export_focus: ExportFocus::Format,
            export_top: "0".to_string(),
            export_depth: "0".to_string(),
            export_error: None,
            export_dir: PathBuf::from("."),
            export_ts: String::new(),
            scanning: true,
            cancel_requested: false,
            paused: false,
            quit: false,
            force_quit: false,
            progress,
            control,
            threads,
            engine_options,
        }
    }

    pub fn finish(&mut self, outcome: ScanOutcome) {
        let cancelled = outcome.cancelled;
        self.outcome = Some(outcome);
        self.scanning = false;
        let root = self.outcome.as_ref().map(|o| o.tree.root).unwrap_or(0);
        self.stack = vec![root];
        self.sel = 0;
        self.rebuild();
        if cancelled {
            self.message = Some("走査を中止しました (入手できた部分結果を表示中)".into());
        }
    }

    /// 走査が内部エラーで失敗した場合。
    pub fn fail(&mut self, message: String) {
        self.scanning = false;
        self.outcome = None;
        self.listing.clear();
        self.stack = vec![0];
        self.message = Some(message);
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.outcome.as_ref().map(|o| &o.tree)
    }

    pub fn cur(&self) -> u32 {
        *self.stack.last().unwrap_or(&0)
    }

    pub fn metric_label(&self) -> &'static str {
        if self.show_allocated {
            "allocated"
        } else {
            "logical"
        }
    }

    /// 画面表示用の指標名。
    pub fn metric_display_label(&self) -> &'static str {
        if self.show_allocated {
            "ディスク使用量"
        } else {
            "見かけのサイズ"
        }
    }

    pub fn current_path(&self) -> String {
        let Some(t) = self.tree() else {
            return self.root_display.clone();
        };
        let mut out = String::new();
        for (k, &i) in self.stack.iter().enumerate() {
            if k == 0 {
                out.push_str(&t.node(i).name.to_string_lossy());
            } else {
                out.push(std::path::MAIN_SEPARATOR);
                out.push_str(&t.node(i).name.to_string_lossy());
            }
        }
        out
    }

    pub fn rebuild(&mut self) {
        let Some(t) = self.tree() else { return };
        let cur = self.cur();
        let mut kids: Vec<u32> = t.node(cur).children.clone();
        let f = self.filter.to_lowercase();
        if !f.is_empty() {
            kids.retain(|&i| t.node(i).name.to_string_lossy().to_lowercase().contains(&f));
        }
        let field = self.sort_field;
        let dir = self.sort_dir;
        let show_alloc = self.show_allocated;
        kids.sort_by(|&a, &b| sort::compare_nodes(t, a, b, field, dir, show_alloc));
        self.listing = kids;
        if self.sel >= self.listing.len() {
            self.sel = self.listing.len().saturating_sub(1);
        }
    }

    pub fn sort_label(&self) -> String {
        format!("{}{}", self.sort_field.label(), self.sort_dir.arrow())
    }

    fn enter(&mut self) {
        if self.scanning {
            return;
        }
        let idx = match self.listing.get(self.sel) {
            Some(&i) => i,
            None => return,
        };
        let is_dir = self.tree().map(|t| t.node(idx).kind) == Some(NodeKind::Dir);
        if is_dir {
            self.stack.push(idx);
            self.sel = 0;
            self.rebuild();
        } else {
            self.message = Some("フォルダではありません".into());
        }
    }

    fn go_up(&mut self) {
        if self.scanning {
            return;
        }
        if self.stack.len() > 1 {
            self.stack.pop();
            self.sel = 0;
            self.rebuild();
        } else {
            self.message = Some("これ以上上へは移動できません".into());
        }
    }

    pub fn tree_mut(&mut self) -> Option<&mut Tree> {
        self.outcome.as_mut().map(|o| &mut o.tree)
    }

    /// ノードの実パスをルートから順に組み立てる。
    fn path_of(&self, idx: u32) -> PathBuf {
        let Some(t) = self.tree() else {
            return self.root.clone();
        };
        let mut names = Vec::new();
        let mut cur = idx;
        while cur != t.root {
            let n = t.node(cur);
            if n.parent == u32::MAX {
                break;
            }
            names.push(n.name.clone());
            cur = n.parent;
        }
        let mut path = self.root.clone();
        for name in names.into_iter().rev() {
            path.push(name.as_ref());
        }
        path
    }

    /// 選択項目の削除確認を開く。
    fn begin_delete(&mut self) {
        if !self.allow_delete {
            self.message = Some("削除するには起動時に --allow-delete を指定してください".into());
            return;
        }
        if self.scanning {
            self.message = Some("走査中は削除できません".into());
            return;
        }
        let Some(&idx) = self.listing.get(self.sel) else {
            self.message = Some("削除する項目がありません".into());
            return;
        };
        let Some(t) = self.tree() else {
            self.message = Some("削除する結果がありません".into());
            return;
        };
        if idx == t.root {
            return;
        }
        let n = t.node(idx);
        let (recorded_kind, bytes, files, dirs, truncated) = (
            n.kind,
            if self.show_allocated {
                n.total_alloc
            } else {
                n.total_logical
            },
            n.file_count,
            n.dir_count,
            n.truncated,
        );
        let path = self.path_of(idx);
        // --follow-links でフォルダとして記録されたリンクでも、表示と削除は
        // リンクとして扱う (リンク先は削除しない)。
        let mut kind = recorded_kind;
        if let Ok(md) = std::fs::symlink_metadata(&path) {
            if md.file_type().is_symlink() || platform::is_reparse_point(&md) {
                kind = NodeKind::Other;
            }
        }
        // リンク先の中身をツリーに持つリンクを消すと、リンク先が別のパスに
        // 残っている場合に合計がずれる。安全のため削除を拒否する。
        if kind == NodeKind::Other && !n.children.is_empty() {
            self.message = Some(
                "--follow-links で辿ったリンクは削除できません (リンク先を直接開いてください)"
                    .into(),
            );
            return;
        }
        self.delete_target = Some(DeleteTarget {
            node: idx,
            path,
            kind,
            bytes,
            files,
            dirs,
            truncated,
            input: String::new(),
            error: None,
        });
        self.mode = Mode::ConfirmDelete;
    }

    fn on_key_confirm_delete(&mut self, key: KeyEvent) {
        use KeyCode::*;
        let is_dir = self
            .delete_target
            .as_ref()
            .is_some_and(|t| t.kind == NodeKind::Dir);
        match key.code {
            Esc => {
                self.delete_target = None;
                self.mode = Mode::Normal;
                self.message = Some("削除をキャンセルしました".into());
            }
            Char('y') | Char('Y') if !is_dir => self.perform_delete(),
            Enter if is_dir => {
                let confirmed = self
                    .delete_target
                    .as_ref()
                    .is_some_and(|t| t.input == "yes");
                if confirmed {
                    self.perform_delete();
                } else if let Some(t) = self.delete_target.as_mut() {
                    t.error = Some("確認のため yes と入力してください".into());
                }
            }
            Backspace if is_dir => {
                if let Some(t) = self.delete_target.as_mut() {
                    t.input.pop();
                    t.error = None;
                }
            }
            Char(c) if is_dir && !c.is_control() => {
                if let Some(t) = self.delete_target.as_mut() {
                    if t.input.chars().count() < 16 {
                        t.input.push(c);
                    }
                    t.error = None;
                }
            }
            _ => {}
        }
    }

    fn perform_delete(&mut self) {
        let Some(target) = self.delete_target.take() else {
            self.mode = Mode::Normal;
            return;
        };
        match delete::remove_target(&target.path, target.kind) {
            Ok(()) => {
                if let Some(out) = self.outcome.as_mut() {
                    // 再帰削除するのは記録上 Dir のときだけ。リンクや Blocked は
                    // 実体だけを消すため、子ノードを削除済みとして扱わない。
                    let removed: HashSet<u32> = if target.kind == NodeKind::Dir {
                        subtree_indices(&out.tree, target.node)
                            .into_iter()
                            .collect()
                    } else {
                        std::iter::once(target.node).collect()
                    };
                    if out.tree.detach(target.node) {
                        // 削除した側がハードリンクの実体を計上していた場合、
                        // 残存リンクへ割当サイズを移す。
                        let mut groups = std::mem::take(&mut out.hardlink_groups);
                        for nodes in groups.values_mut() {
                            let owner = nodes
                                .iter()
                                .copied()
                                .find(|&n| removed.contains(&n) && out.tree.node(n).own_alloc > 0);
                            let survivor = nodes.iter().copied().find(|&n| !removed.contains(&n));
                            if let (Some(owner), Some(survivor)) = (owner, survivor) {
                                let amount = out.tree.node(owner).own_alloc;
                                out.tree.add_alloc(survivor, amount);
                            }
                            nodes.retain(|n| !removed.contains(n));
                        }
                        groups.retain(|_, nodes| nodes.len() > 1);
                        out.hardlink_groups = groups;
                    }
                }
                self.rebuild();
                self.mode = Mode::Normal;
                self.message = Some(format!(
                    "削除しました: {} ({})",
                    target.path.display(),
                    self.size_format.format(target.bytes)
                ));
            }
            Err(e) => {
                self.mode = Mode::Normal;
                self.message = Some(format!(
                    "削除できませんでした (一部のみ削除された可能性があります): {}: {e}",
                    target.path.display()
                ));
            }
        }
    }

    /// 保存設定フォームを開く。
    fn begin_export(&mut self) {
        if self.outcome.is_none() {
            self.message = Some("保存する結果がありません".into());
            return;
        }
        self.export_sel = 0;
        self.export_focus = ExportFocus::Format;
        self.export_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.export_ts = report::timestamp_slug();
        self.export_error = None;
        let format = self.current_export_format();
        self.reset_export_values(format);
        self.mode = Mode::ExportForm;
    }

    pub fn current_export_format(&self) -> OutputFormat {
        EXPORT_FORMATS[self.export_sel.min(EXPORT_FORMATS.len() - 1)]
    }

    fn reset_export_values(&mut self, format: OutputFormat) {
        self.export_top = format.default_top().to_string();
        self.export_depth = format.default_depth().to_string();
    }

    fn change_export_format(&mut self, delta: isize) {
        let len = EXPORT_FORMATS.len() as isize;
        let idx = (self.export_sel as isize + delta).rem_euclid(len) as usize;
        if idx != self.export_sel {
            self.export_sel = idx;
            let format = EXPORT_FORMATS[idx];
            self.reset_export_values(format);
            self.export_error = None;
        }
    }

    /// Tab / Shift+Tab の移動。無効な項目は飛ばす。
    fn focus_next_export(&mut self, forward: bool) {
        let format = self.current_export_format();
        let mut order = vec![ExportFocus::Format];
        if format.uses_top() {
            order.push(ExportFocus::Top);
        }
        if format.uses_depth() {
            order.push(ExportFocus::Depth);
        }
        order.push(ExportFocus::Save);
        let pos = order
            .iter()
            .position(|&f| f == self.export_focus)
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % order.len()
        } else {
            (pos + order.len() - 1) % order.len()
        };
        self.export_focus = order[next];
    }

    fn bump_export_number(&mut self, delta: i64) {
        let value = match self.export_focus {
            ExportFocus::Top => &mut self.export_top,
            ExportFocus::Depth => &mut self.export_depth,
            _ => return,
        };
        let cur = value.parse::<usize>().unwrap_or(0);
        let next = if delta < 0 {
            cur.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            cur.saturating_add(delta as usize)
        };
        *value = next.min(999_999_999).to_string();
        self.export_error = None;
    }

    fn on_key_export_form(&mut self, key: KeyEvent) {
        use KeyCode::*;
        match key.code {
            Tab => self.focus_next_export(true),
            BackTab => self.focus_next_export(false),
            Esc => self.cancel_export(),
            Enter => {
                if self.export_focus == ExportFocus::Save {
                    self.save_export_form();
                } else {
                    self.focus_next_export(true);
                }
            }
            Left | Char('h') => {
                if self.export_focus == ExportFocus::Format {
                    self.change_export_format(-1);
                }
            }
            Right | Char('l') => {
                if self.export_focus == ExportFocus::Format {
                    self.change_export_format(1);
                }
            }
            Up => match self.export_focus {
                ExportFocus::Format => self.change_export_format(-1),
                ExportFocus::Top | ExportFocus::Depth => self.bump_export_number(1),
                ExportFocus::Save => {}
            },
            Down => match self.export_focus {
                ExportFocus::Format => self.change_export_format(1),
                ExportFocus::Top | ExportFocus::Depth => self.bump_export_number(-1),
                ExportFocus::Save => {}
            },
            Backspace => {
                if matches!(self.export_focus, ExportFocus::Top | ExportFocus::Depth) {
                    let value = if self.export_focus == ExportFocus::Top {
                        &mut self.export_top
                    } else {
                        &mut self.export_depth
                    };
                    value.pop();
                    self.export_error = None;
                }
            }
            Char(c) if c.is_ascii_digit() => {
                if matches!(self.export_focus, ExportFocus::Top | ExportFocus::Depth) {
                    let value = if self.export_focus == ExportFocus::Top {
                        &mut self.export_top
                    } else {
                        &mut self.export_depth
                    };
                    if value.len() < 9 {
                        value.push(c);
                    }
                    self.export_error = None;
                }
            }
            _ => {}
        }
    }

    pub fn export_preview_path(&self) -> PathBuf {
        let format = self.current_export_format();
        self.export_dir
            .join(format!("dskr-{}.{}", self.export_ts, format.extension()))
    }

    fn build_export_request(&self) -> Result<ExportRequest, String> {
        let format = self.current_export_format();
        let top = if format.uses_top() {
            parse_export_number(&self.export_top, "件数")?
        } else {
            0
        };
        let depth = if format.uses_depth() {
            parse_export_number(&self.export_depth, "深さ")?
        } else {
            0
        };
        let path = unique_export_path(&self.export_dir, &self.export_ts, format)?;
        Ok(ExportRequest {
            format,
            top,
            depth,
            path,
        })
    }

    fn save_export_form(&mut self) {
        let req = match self.build_export_request() {
            Ok(req) => req,
            Err(e) => {
                self.export_error = Some(e);
                return;
            }
        };
        match self.write_export(&req) {
            Ok(()) => {
                self.mode = Mode::Normal;
                self.export_error = None;
                self.message = Some(format!(
                    "保存しました: {} ({})",
                    req.path.display(),
                    req.format.label()
                ));
            }
            Err(e) => {
                self.export_error = Some(format!("保存に失敗: {e}"));
            }
        }
    }

    fn cancel_export(&mut self) {
        self.mode = Mode::Normal;
        self.export_error = None;
        self.message = Some("保存をキャンセルしました".into());
    }

    fn write_export(&mut self, req: &ExportRequest) -> Result<(), String> {
        let Some(out) = self.outcome.as_ref() else {
            return Err("保存する結果がありません".into());
        };
        let file = report::create_private(&req.path, true).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("既に存在します: {}", req.path.display())
            } else {
                e.to_string()
            }
        })?;
        let meta = report::build_meta(
            out,
            &self.root_display,
            self.root_kind.label(),
            &self.profile_label,
            self.threads,
            &self.engine_options,
            self.metric_label(),
            self.alloc_collected,
        );
        let opts = OutputOptions {
            format: req.format,
            depth: req.depth,
            top: req.top,
            sort_field: self.sort_field,
            sort_dir: self.sort_dir,
            size: self.size_format,
            ascii: false,
            color: false,
            metric_alloc: self.show_allocated,
        };
        let mut w = std::io::BufWriter::new(file);
        output::write_output(&mut w, out, &meta, &opts).map_err(|e| e.to_string())?;
        std::io::Write::flush(&mut w).map_err(|e| e.to_string())
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.force_quit = true;
            return;
        }
        match self.mode {
            Mode::Filter => self.on_key_filter(key),
            Mode::Help => self.on_key_help(key),
            Mode::ConfirmQuit => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.force_quit = true,
                _ => self.mode = Mode::Normal,
            },
            Mode::ExportForm => self.on_key_export_form(key),
            Mode::ConfirmDelete => self.on_key_confirm_delete(key),
            Mode::Errors => self.on_key_errors(key),
            Mode::Normal => self.on_key_normal(key),
        }
    }

    fn on_key_help(&mut self, key: KeyEvent) {
        use KeyCode::*;
        match key.code {
            Esc | Char('?') | Char('q') => self.mode = Mode::Normal,
            Up | Char('k') => self.help_scroll = self.help_scroll.saturating_sub(1),
            Down | Char('j') => self.help_scroll = (self.help_scroll + 1).min(self.help_max_scroll),
            PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
            PageDown => self.help_scroll = (self.help_scroll + 10).min(self.help_max_scroll),
            Home | Char('g') => self.help_scroll = 0,
            End | Char('G') => self.help_scroll = self.help_max_scroll,
            _ => {}
        }
    }

    fn on_key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.sel = 0;
                self.rebuild();
                self.message = Some(format!(
                    "絞込: \"{}\" ({} 件表示)",
                    self.filter,
                    self.listing.len()
                ));
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
                self.sel = 0;
                self.rebuild();
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
            }
            _ => {}
        }
    }

    fn on_key_errors(&mut self, key: KeyEvent) {
        let count = self.outcome.as_ref().map(|o| o.errors.len()).unwrap_or(0);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.err_sel = self.err_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.err_sel = (self.err_sel + 1).min(count.saturating_sub(1));
            }
            KeyCode::Esc | KeyCode::Char('E') | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        use KeyCode::*;
        if self.scanning {
            match key.code {
                Char(' ') => match self.control.state() {
                    ControlState::Paused => {
                        self.control.resume();
                        self.paused = false;
                        self.message = Some("再開しました".into());
                    }
                    _ => {
                        self.control.pause();
                        self.paused = true;
                        self.message = Some("一時停止しました".into());
                    }
                },
                Esc => {
                    self.control.cancel();
                    self.cancel_requested = true;
                    self.message =
                        Some("中止要求を送信しました (完了後も部分結果を表示できます)".into());
                }
                Char('q') => {
                    if !self.cancel_requested {
                        self.control.cancel();
                        self.cancel_requested = true;
                    }
                    self.mode = Mode::ConfirmQuit;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            Char('q') => self.quit = true,
            Up | Char('k') => {
                self.sel = self.sel.saturating_sub(1);
            }
            Down | Char('j') => {
                self.sel = (self.sel + 1).min(self.listing.len().saturating_sub(1));
            }
            Home | Char('g') => self.sel = 0,
            End | Char('G') => self.sel = self.listing.len().saturating_sub(1),
            Enter | Right | Char('l') => self.enter(),
            Left | Backspace | Char('h') => self.go_up(),
            Char('s') => {
                self.sort_field = self.sort_field.next();
                self.rebuild();
                self.message = Some(format!("並び順: {} (S で昇順/降順)", self.sort_label()));
            }
            Char('S') => {
                self.sort_dir = self.sort_dir.toggled();
                self.rebuild();
                self.message = Some(format!("並び順: {}", self.sort_label()));
            }
            Char('u') => {
                self.size_format.cycle_fixed();
                self.message = Some(format!("単位: {}", self.size_format.display_label()));
            }
            Char('U') => {
                self.size_format.toggle_system();
                self.message = Some(format!(
                    "単位系: {} ({})",
                    self.size_format.system_label(),
                    self.size_format.system_description()
                ));
            }
            Char('a') => {
                if !self.show_allocated && !self.alloc_collected {
                    self.message = Some(
                        "このプロファイルではディスク使用量を取得していません (見かけのサイズのみ)"
                            .into(),
                    );
                    return;
                }
                self.show_allocated = !self.show_allocated;
                self.rebuild();
                self.message = Some(format!("合計: {}", self.metric_display_label()));
            }
            Char('/') => {
                self.filter.clear();
                self.mode = Mode::Filter;
            }
            Char('E') => {
                self.mode = Mode::Errors;
                self.err_sel = 0;
            }
            Char('e') => self.begin_export(),
            Char('d') => self.begin_delete(),
            Char('?') => {
                self.help_scroll = 0;
                self.mode = Mode::Help;
            }
            _ => {}
        }
    }
}

fn parse_export_number(value: &str, label: &str) -> Result<usize, String> {
    if value.is_empty() {
        return Err(format!("{label}を入力してください"));
    }
    value
        .parse::<usize>()
        .map_err(|_| format!("{label}が大きすぎます"))
}

/// 既存ファイルと衝突しない保存先を決める。
fn unique_export_path(
    dir: &std::path::Path,
    ts: &str,
    format: OutputFormat,
) -> Result<PathBuf, String> {
    let ext = format.extension();
    for i in 0..100u32 {
        let candidate = if i == 0 {
            dir.join(format!("dskr-{ts}.{ext}"))
        } else {
            dir.join(format!("dskr-{ts}-{i}.{ext}"))
        };
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err("出力ファイル名を決定できません".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::node::{Node, Tree};
    use std::time::Duration;

    fn test_app() -> App {
        App::new(
            PathBuf::from("/tmp"),
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

    fn finish_scan(app: &mut App) {
        let mut tree = Tree::with_root("/tmp".into(), 0);
        let dir = tree
            .push_child_capped(0, Node::dir("sub".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(
            dir,
            Node::file("a.bin".into(), dir, 2, 100, 100, 1_704_067_200),
            100,
        )
        .unwrap();
        tree.aggregate();
        app.finish(ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 3,
            dirs: 2,
            error_count: 0,
            bytes_alloc: 100,
            bytes_logical: 100,
            elapsed: Duration::from_millis(5),
            root: PathBuf::from("/tmp"),
        });
    }

    fn test_app_at(root: &std::path::Path, allow_delete: bool) -> App {
        App::new(
            root.to_path_buf(),
            RootKind::Local,
            "local".to_string(),
            true,
            allow_delete,
            Arc::new(Progress::new()),
            Arc::new(Control::new()),
            2,
            "{}".to_string(),
        )
    }

    /// 実ディレクトリを 1 階層分だけ読み取ってツリーを作る。
    fn finish_real_scan(app: &mut App, root: &std::path::Path) {
        let mut tree = Tree::with_root(root.as_os_str().to_owned(), 0);
        let mut entries: Vec<_> = std::fs::read_dir(root).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name();
            let md = entry.metadata().unwrap();
            if md.is_dir() {
                let d = tree
                    .push_child_capped(0, Node::dir(name, 0, 1, 0), 100)
                    .unwrap();
                for child in std::fs::read_dir(entry.path()).unwrap().flatten() {
                    let cmd = child.metadata().unwrap();
                    let n = Node::file(child.file_name(), d, 2, cmd.len(), cmd.len(), 0);
                    tree.push_child_capped(d, n, 100).unwrap();
                }
            } else {
                tree.push_child_capped(0, Node::file(name, 0, 1, md.len(), md.len(), 0), 100)
                    .unwrap();
            }
        }
        tree.aggregate();
        app.finish(ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 0,
            dirs: 0,
            error_count: 0,
            bytes_alloc: 0,
            bytes_logical: 0,
            elapsed: Duration::from_millis(0),
            root: root.to_path_buf(),
        });
    }

    fn select_name(app: &mut App, name: &str) {
        let idx = {
            let t = app.tree().unwrap();
            let cur = app.cur();
            t.node(cur)
                .children
                .iter()
                .copied()
                .find(|&i| t.node(i).name.to_string_lossy() == name)
                .unwrap_or_else(|| panic!("{name} がありません"))
        };
        app.sel = app
            .listing
            .iter()
            .position(|&i| i == idx)
            .unwrap_or_else(|| panic!("{name} が一覧にありません"));
    }

    #[test]
    fn path_of_builds_nested_path_from_root() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("a.bin"), b"x").unwrap();
        let mut app = test_app_at(dir.path(), true);
        finish_real_scan(&mut app, dir.path());

        let t = app.tree().unwrap();
        let victim_idx = t
            .node(0)
            .children
            .iter()
            .copied()
            .find(|&i| t.node(i).name.to_string_lossy() == "victim")
            .unwrap();
        let file_idx = t
            .node(victim_idx)
            .children
            .iter()
            .copied()
            .find(|&i| t.node(i).name.to_string_lossy() == "a.bin")
            .unwrap();

        assert_eq!(
            app.path_of(file_idx),
            dir.path().join("victim").join("a.bin")
        );
    }

    #[cfg(unix)]
    #[test]
    fn deleting_followed_symlink_with_children_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inside"), b"x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut tree = Tree::with_root(dir.path().as_os_str().to_owned(), 0);
        let link_idx = tree
            .push_child_capped(0, Node::dir("link".into(), 0, 1, 0), 100)
            .unwrap();
        tree.push_child_capped(
            link_idx,
            Node::file("inside".into(), link_idx, 2, 10, 1, 0),
            100,
        )
        .unwrap();
        tree.aggregate();

        let mut app = test_app_at(dir.path(), true);
        app.finish(ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: Default::default(),
            truncated: false,
            cancelled: false,
            entries: 2,
            dirs: 1,
            error_count: 0,
            bytes_alloc: 10,
            bytes_logical: 1,
            elapsed: Duration::from_millis(1),
            root: dir.path().to_path_buf(),
        });

        app.sel = app
            .listing
            .iter()
            .position(|&i| app.tree().unwrap().node(i).name.to_string_lossy() == "link")
            .unwrap();
        app.begin_delete();

        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.delete_target.is_none());
        assert!(link.exists());
        assert!(
            app.message
                .as_deref()
                .unwrap_or("")
                .contains("--follow-links"),
            "{:?}",
            app.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn deleting_hardlink_owner_moves_allocation_to_survivor() {
        use std::collections::HashMap;

        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("f1");
        let f2 = dir.path().join("f2");
        std::fs::write(&f1, b"x").unwrap();
        std::fs::hard_link(&f1, &f2).unwrap();

        let mut tree = Tree::with_root(dir.path().as_os_str().to_owned(), 0);
        let n1 = tree
            .push_child_capped(0, Node::file("f1".into(), 0, 1, 100, 1, 0), 100)
            .unwrap();
        let n2 = tree
            .push_child_capped(0, Node::file("f2".into(), 0, 1, 0, 1, 0), 100)
            .unwrap();
        tree.aggregate();

        let mut groups = HashMap::new();
        groups.insert((1u64, 1u64), vec![n1, n2]);
        let mut app = test_app_at(dir.path(), true);
        app.finish(ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: groups,
            truncated: false,
            cancelled: false,
            entries: 2,
            dirs: 0,
            error_count: 0,
            bytes_alloc: 100,
            bytes_logical: 2,
            elapsed: Duration::from_millis(1),
            root: dir.path().to_path_buf(),
        });

        app.sel = app
            .listing
            .iter()
            .position(|&i| app.tree().unwrap().node(i).name.to_string_lossy() == "f1")
            .unwrap();
        app.begin_delete();
        assert!(matches!(app.mode, Mode::ConfirmDelete));
        app.perform_delete();

        assert!(!f1.exists());
        assert!(f2.exists());
        let tree = app.tree().unwrap();
        assert_eq!(tree.node(n2).own_alloc, 100);
        assert_eq!(tree.node(tree.root).total_alloc, 100);
        assert!(app.outcome.as_ref().unwrap().hardlink_groups.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn deleting_directory_with_hardlink_owner_moves_allocation_outside() {
        use std::collections::HashMap;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let inside = sub.join("inside");
        let outside = dir.path().join("outside");
        std::fs::write(&inside, b"x").unwrap();
        std::fs::hard_link(&inside, &outside).unwrap();

        let mut tree = Tree::with_root(dir.path().as_os_str().to_owned(), 0);
        let sub_idx = tree
            .push_child_capped(0, Node::dir("sub".into(), 0, 1, 0), 100)
            .unwrap();
        let inside_idx = tree
            .push_child_capped(
                sub_idx,
                Node::file("inside".into(), sub_idx, 2, 100, 1, 0),
                100,
            )
            .unwrap();
        let outside_idx = tree
            .push_child_capped(0, Node::file("outside".into(), 0, 1, 0, 1, 0), 100)
            .unwrap();
        tree.aggregate();

        let mut groups = HashMap::new();
        groups.insert((1u64, 1u64), vec![inside_idx, outside_idx]);
        let mut app = test_app_at(dir.path(), true);
        app.finish(ScanOutcome {
            tree,
            errors: Vec::new(),
            hardlink_groups: groups,
            truncated: false,
            cancelled: false,
            entries: 3,
            dirs: 1,
            error_count: 0,
            bytes_alloc: 100,
            bytes_logical: 2,
            elapsed: Duration::from_millis(1),
            root: dir.path().to_path_buf(),
        });

        app.delete_target = Some(DeleteTarget {
            node: sub_idx,
            path: sub.clone(),
            kind: NodeKind::Dir,
            bytes: 100,
            files: 1,
            dirs: 0,
            truncated: false,
            input: "yes".into(),
            error: None,
        });
        app.perform_delete();

        assert!(!sub.exists());
        assert!(outside.exists());
        let tree = app.tree().unwrap();
        assert_eq!(tree.node(outside_idx).own_alloc, 100);
        assert_eq!(tree.node(tree.root).total_alloc, 100);
        assert!(app.outcome.as_ref().unwrap().hardlink_groups.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_network_profile_without_alloc_cannot_toggle_metric() {
        let mut app = App::new(
            PathBuf::from(r"C:\"),
            RootKind::Unc,
            "network".to_string(),
            false,
            false,
            Arc::new(Progress::new()),
            Arc::new(Control::new()),
            2,
            "{}".to_string(),
        );
        assert!(!app.alloc_collected);
        app.scanning = false;
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!app.show_allocated);
        assert!(app.message.as_deref().unwrap_or("").contains("見かけ"));
    }

    #[test]
    fn ctrl_c_requests_force_quit() {
        let mut app = test_app();
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.force_quit);
    }

    #[test]
    fn delete_key_is_ignored_without_option() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("victim.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut app = test_app_at(dir.path(), false);
        finish_real_scan(&mut app, dir.path());
        select_name(&mut app, "victim.txt");

        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.delete_target.is_none());
        assert!(file.exists());
        assert!(app
            .message
            .as_deref()
            .unwrap_or("")
            .contains("--allow-delete"));
    }

    #[test]
    fn delete_file_after_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("victim.txt");
        std::fs::write(&file, b"hello").unwrap();
        let mut app = test_app_at(dir.path(), true);
        finish_real_scan(&mut app, dir.path());
        select_name(&mut app, "victim.txt");

        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ConfirmDelete);
        assert_eq!(
            app.delete_target.as_ref().map(|t| t.path.clone()),
            Some(file.clone())
        );
        app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert!(!file.exists());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.delete_target.is_none());
        assert!(app.listing.is_empty());
        assert!(app
            .message
            .as_deref()
            .unwrap_or("")
            .contains("削除しました"));
        assert_eq!(app.tree().unwrap().node(0).file_count, 0);
        assert_eq!(app.tree().unwrap().node(0).total_alloc, 0);
    }

    #[test]
    fn delete_directory_requires_yes_input() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(victim.join("nested")).unwrap();
        std::fs::write(victim.join("nested").join("a.bin"), b"x").unwrap();
        let mut app = test_app_at(dir.path(), true);
        finish_real_scan(&mut app, dir.path());
        select_name(&mut app, "victim");

        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ConfirmDelete);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ConfirmDelete);
        assert!(app
            .delete_target
            .as_ref()
            .and_then(|t| t.error.as_deref())
            .unwrap_or("")
            .contains("yes"));
        assert!(victim.exists());

        for c in ['y', 'e', 's'] {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!victim.exists());
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.listing.is_empty());
    }

    #[test]
    fn cancel_delete_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("victim.txt");
        std::fs::write(&file, b"x").unwrap();
        let mut app = test_app_at(dir.path(), true);
        finish_real_scan(&mut app, dir.path());
        select_name(&mut app, "victim.txt");

        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.delete_target.is_none());
        assert!(file.exists());
        assert_eq!(app.listing.len(), 1);
        assert!(app.message.as_deref().unwrap_or("").contains("キャンセル"));
    }

    #[test]
    fn writes_every_export_format() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app();
        finish_scan(&mut app);

        for (i, format) in EXPORT_FORMATS.iter().enumerate() {
            let path = dir.path().join(format!("out-{i}.{}", format.extension()));
            let req = ExportRequest {
                format: *format,
                top: format.default_top(),
                depth: format.default_depth(),
                path: path.clone(),
            };
            app.write_export(&req)
                .unwrap_or_else(|e| panic!("{format:?} の出力に失敗: {e}"));
            let data = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("{format:?} の出力を読めません: {e}");
            });
            assert!(!data.is_empty(), "{format:?}");
            match format {
                OutputFormat::Html => assert!(data.starts_with("<!DOCTYPE html>")),
                OutputFormat::Json => assert!(data.starts_with('{')),
                OutputFormat::Csv => assert!(data.starts_with("path,")),
                OutputFormat::Markdown => assert!(data.starts_with('#')),
                _ => {}
            }
        }
    }

    #[test]
    fn refuses_to_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.json");
        std::fs::write(&path, b"keep").unwrap();

        let mut app = test_app();
        finish_scan(&mut app);
        let err = app
            .write_export(&ExportRequest {
                format: OutputFormat::Json,
                top: 0,
                depth: 0,
                path: path.clone(),
            })
            .unwrap_err();

        assert!(err.contains("既に存在します"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    }

    #[test]
    fn unique_export_path_avoids_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("dskr-20260101-000000.json");
        std::fs::write(&base, b"x").unwrap();

        let path = unique_export_path(dir.path(), "20260101-000000", OutputFormat::Json).unwrap();
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("dskr-20260101-000000-1.json")
        );
    }

    #[test]
    fn export_form_applies_defaults_and_skips_disabled_fields() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app();
        finish_scan(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ExportForm);
        assert_eq!(app.export_focus, ExportFocus::Format);
        // 既定は JSON で top/depth は無効。
        assert_eq!(app.export_top, "0");
        assert_eq!(app.export_depth, "0");
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Save);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Format);

        // JSON → Markdown で既定値が入る。
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.current_export_format(), OutputFormat::Markdown);
        assert_eq!(app.export_top, "10");
        assert_eq!(app.export_depth, "2");

        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Top);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Depth);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Save);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Format);
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert_eq!(app.export_focus, ExportFocus::Save);

        // Markdown → JSON では top/depth がスキップされる。
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Format);
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.current_export_format(), OutputFormat::Json);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Save);
    }

    #[test]
    fn export_form_edits_numbers_and_validates() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = test_app();
        finish_scan(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_top, "10");

        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.export_top, "1");
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.export_top, "");
        app.on_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
        assert_eq!(app.export_top, "5");
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.export_top, "6");
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.export_top, "5");

        // 空欄のまま保存しようとするとエラーになり、フォームに留まる。
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Save);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ExportForm);
        assert!(app.export_error.as_deref().unwrap_or("").contains("件数"));
    }

    #[test]
    fn export_form_saves_with_custom_values() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app();
        finish_scan(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.export_dir = dir.path().to_path_buf();
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)); // Markdown
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.export_top = "1".to_string();
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.export_depth = "1".to_string();
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.export_focus, ExportFocus::Save);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.mode, Mode::Normal);
        assert!(app
            .message
            .as_deref()
            .unwrap_or("")
            .contains("保存しました"));

        let md = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
            .expect("markdown 出力");
        let data = std::fs::read_to_string(md).unwrap();
        // depth=1 ではツリーに sub/ の下の a.bin は出ない (上位一覧には出る)。
        assert!(data.contains("フォルダ構成 (深さ 1)"), "{data}");
        let tree = &data[data.find("<pre>").expect("<pre>")..];
        assert!(!tree.contains("a.bin"), "{tree}");
    }
}
