//! 並列スキャンエンジン。カスタムワーカープールにより、一時停止・キャンセル、
//! レート制限 (エントリ/stat 処理数ベース) を確実に制御する。

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};

use super::node::{Node, Tree};
use super::platform;
use crate::path;

pub const MAX_RECORDED_ERRORS: usize = 1000;
/// ディレクトリタスクキューは無限だが、キューに溜まるのはディレクトリのみ
/// (ファイルはバッチで直接ツリーへ追加する)。メモリ上限の目安は max_nodes。
pub const MAX_NODES_DEFAULT: u64 = 5_000_000;

/// ロック取得。poison されていても中身を取り出して継続する
/// (ワーカーのパニックで全体が連鎖停止しないように)。
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlState {
    Running,
    Paused,
    Cancelling,
}

pub struct Control {
    state: Mutex<ControlState>,
    cv: Condvar,
}

impl Control {
    pub fn new() -> Self {
        Control {
            state: Mutex::new(ControlState::Running),
            cv: Condvar::new(),
        }
    }

    pub fn state(&self) -> ControlState {
        *lock(&self.state)
    }

    pub fn pause(&self) {
        let mut s = lock(&self.state);
        if *s == ControlState::Running {
            *s = ControlState::Paused;
        }
    }

    pub fn resume(&self) {
        let mut s = lock(&self.state);
        if *s == ControlState::Paused {
            *s = ControlState::Running;
        }
        drop(s);
        self.cv.notify_all();
    }

    pub fn cancel(&self) {
        let mut s = lock(&self.state);
        if *s != ControlState::Cancelling {
            *s = ControlState::Cancelling;
        }
        drop(s);
        self.cv.notify_all();
    }

    /// 一時停止中なら再開またはキャンセルされるまでブロックする。
    fn wait_if_paused(&self) {
        let mut s = lock(&self.state);
        while *s == ControlState::Paused {
            s = self.cv.wait(s).unwrap();
        }
    }
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub dirs: AtomicU64,
    pub errors: AtomicU64,
    pub bytes_alloc: AtomicU64,
    pub bytes_logical: AtomicU64,
    pub current: Mutex<String>,
    pub started: Mutex<Option<Instant>>,
}

impl Progress {
    pub fn new() -> Self {
        Progress {
            started: Mutex::new(Some(Instant::now())),
            ..Default::default()
        }
    }

    pub fn elapsed(&self) -> Duration {
        lock(&self.started).map(|t| t.elapsed()).unwrap_or_default()
    }

    pub fn current_dir(&self) -> String {
        lock(&self.current).clone()
    }
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub root: PathBuf,
    pub follow_links: bool,
    pub one_file_system: bool,
    pub excludes: Vec<String>,
    /// 隠しファイル・システムファイルも走査対象にする (--hidden)。
    /// false のときは一覧・集計の両方から除外する。
    pub include_hidden: bool,
    pub min_size: u64,
    pub max_depth: Option<u16>,
    pub max_nodes: u64,
    pub threads: usize,
    pub rate_limit: u64,
    pub skip_cloud_placeholders: bool,
    pub want_allocated: bool,
    pub skip_pseudo: bool,
    /// ネットワークプロファイル (UNC / マウントされたネットワークFS)。
    /// Windows では追加のネットワーク往復を避けるためハードリンク判定を省略し、
    /// リンク追従は同一ボリュームに制限する。
    pub network: bool,
    /// --local-only: ネットワークFSへの走査を一切行わない。
    pub local_only: bool,
}

pub struct ScanError {
    pub path: String,
    pub message: String,
}

pub struct ScanOutcome {
    pub tree: Tree,
    pub errors: Vec<ScanError>,
    pub truncated: bool,
    pub cancelled: bool,
    pub entries: u64,
    pub dirs: u64,
    pub error_count: u64,
    pub bytes_alloc: u64,
    pub bytes_logical: u64,
    pub elapsed: Duration,
    pub root: PathBuf,
}

enum Msg {
    Dir {
        path: PathBuf,
        node: u32,
        depth: u16,
    },
    Stop,
}

struct Shared {
    cfg: EngineConfig,
    tx: Sender<Msg>,
    pending: AtomicU64,
    tree: Arc<Mutex<Tree>>,
    hardlinks: Mutex<HashMap<(u64, u64), ()>>,
    visited: Mutex<HashSet<(u64, u64)>>,
    progress: Arc<Progress>,
    control: Arc<Control>,
    pacer: Pacer,
    errors: Arc<Mutex<Vec<ScanError>>>,
    root_volume: u64,
    /// ルートのドライブレター (Windows のリンク越境判定用)
    #[cfg_attr(not(windows), allow(dead_code))]
    root_drive: Option<u8>,
    truncated: AtomicBool,
    /// ボリュームID → ネットワークFSか (--local-only 時のみ使用)
    #[cfg_attr(windows, allow(dead_code))]
    netfs_cache: Mutex<HashMap<u64, bool>>,
}

/// 200ms 窓のトークンバケット。rate=0 は無制限。
/// 低レート指定 (50/s等) でも窓あたり最低1トークンを保証する。
/// 窓の計算には単調時計を使う (壁時計の NTP 調整に影響されない)。
struct Pacer {
    tokens_per_window: u64,
    window_ms: u64,
    start: Instant,
    inner: Mutex<(u64, u64)>, // (window, tokens)
}

impl Pacer {
    fn new(rate: u64) -> Self {
        let window_ms = 200;
        let mut tokens_per_window = rate.saturating_mul(window_ms) / 1000;
        if rate > 0 {
            tokens_per_window = tokens_per_window.max(1);
        }
        Pacer {
            tokens_per_window,
            window_ms,
            start: Instant::now(),
            inner: Mutex::new((0, 0)),
        }
    }

    fn acquire(&self) {
        if self.tokens_per_window == 0 {
            return;
        }
        loop {
            let now = self.start.elapsed().as_millis() as u64;
            let w = now / self.window_ms;
            {
                let mut g = lock(&self.inner);
                if g.0 != w {
                    *g = (w, self.tokens_per_window);
                }
                if g.1 > 0 {
                    g.1 -= 1;
                    return;
                }
            }
            let next_ms = (w + 1) * self.window_ms;
            let sleep_ms = next_ms.saturating_sub(now).clamp(1, 20);
            std::thread::sleep(Duration::from_millis(sleep_ms));
        }
    }
}

/// ルートパスのドライブレター (Windows のみ)。
fn root_drive_of(p: &Path) -> Option<u8> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(pc)) = p.components().next() {
            match pc.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => return Some(d),
                _ => {}
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        None
    }
}

/// スキャンを実行し、完了 (またはキャンセル) までブロックする。
/// TUI からは別スレッドで呼び出すことを想定。
pub fn run(cfg: EngineConfig, progress: Arc<Progress>, control: Arc<Control>) -> ScanOutcome {
    let threads = cfg.threads.max(1);
    let (tx, rx) = unbounded::<Msg>();

    // Windows の長いパス (>260) は拡張長表記で走査する。表示・レポートは元の表記。
    #[cfg(windows)]
    let fs_root: PathBuf = path::to_extended(&cfg.root);
    #[cfg(not(windows))]
    let fs_root: PathBuf = cfg.root.clone();

    let root_md = fs::metadata(&fs_root).ok();
    let root_mtime = root_md.as_ref().map(platform::mtime_secs).unwrap_or(0);
    let root_volume = platform::volume_of(&fs_root).unwrap_or(0);

    let tree_arc = Arc::new(Mutex::new(Tree::with_root(
        cfg.root.as_os_str().to_owned(),
        root_mtime,
    )));
    let root = lock(&tree_arc).root;

    let mut visited = HashSet::new();
    if cfg.follow_links {
        if let Some(md) = &root_md {
            if let Some(k) = platform::dir_key(md, &fs_root) {
                visited.insert(k);
            }
        }
    }

    let errors_arc: Arc<Mutex<Vec<ScanError>>> = Arc::new(Mutex::new(Vec::new()));

    let shared = Arc::new(Shared {
        tx: tx.clone(),
        pending: AtomicU64::new(0),
        tree: tree_arc,
        hardlinks: Mutex::new(HashMap::new()),
        visited: Mutex::new(visited),
        progress: progress.clone(),
        control: control.clone(),
        pacer: Pacer::new(cfg.rate_limit),
        errors: errors_arc,
        root_volume,
        root_drive: root_drive_of(&fs_root),
        truncated: AtomicBool::new(false),
        netfs_cache: Mutex::new(HashMap::new()),
        cfg,
    });

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let sh = shared.clone();
        let rx = rx.clone();
        match std::thread::Builder::new()
            .name("dskr-worker".into())
            .spawn(move || worker(sh, rx))
        {
            Ok(h) => handles.push(h),
            Err(e) => {
                eprintln!("警告: ワーカースレッドを作成できません: {e}");
                break;
            }
        }
    }

    if handles.is_empty() {
        shared.record_error(
            Path::new("<internal>"),
            "ワーカースレッドを作成できませんでした".into(),
            None,
        );
    } else {
        // ルートタスク投入
        shared.pending.fetch_add(1, Ordering::Release);
        let _ = shared.tx.send(Msg::Dir {
            path: fs_root.clone(),
            node: root,
            depth: 0,
        });

        // 完了 or キャンセル待ち
        loop {
            if shared.pending.load(Ordering::Acquire) == 0 {
                break;
            }
            if shared.control.state() == ControlState::Cancelling {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // ワーカー停止
        for _ in 0..handles.len() {
            let _ = tx.send(Msg::Stop);
        }
    }
    drop(tx);
    for h in handles {
        let _ = h.join();
    }

    let cancelled = shared.control.state() == ControlState::Cancelling;

    // ワーカーは join 済みなので shared への参照はこのスレッドのみ。
    // 所有権を取り出してツリーを回収する。
    let shared =
        Arc::try_unwrap(shared).unwrap_or_else(|_| panic!("scan: shared reference leaked"));
    let mut tree = Arc::try_unwrap(shared.tree)
        .ok()
        .map(|m| m.into_inner().unwrap_or_else(PoisonError::into_inner))
        .expect("scan: tree");
    tree.aggregate();
    let errors = Arc::try_unwrap(shared.errors)
        .ok()
        .map(|m| m.into_inner().unwrap_or_else(PoisonError::into_inner))
        .expect("scan: errors");

    ScanOutcome {
        entries: shared.progress.entries.load(Ordering::Relaxed),
        dirs: shared.progress.dirs.load(Ordering::Relaxed),
        error_count: shared.progress.errors.load(Ordering::Relaxed),
        bytes_alloc: shared.progress.bytes_alloc.load(Ordering::Relaxed),
        bytes_logical: shared.progress.bytes_logical.load(Ordering::Relaxed),
        elapsed: shared.progress.elapsed(),
        tree,
        errors,
        truncated: shared.truncated.load(Ordering::Relaxed),
        cancelled,
        root: shared.cfg.root.clone(),
    }
}

fn worker(sh: Arc<Shared>, rx: crossbeam_channel::Receiver<Msg>) {
    loop {
        if sh.control.state() == ControlState::Cancelling {
            break;
        }
        match rx.recv() {
            Ok(Msg::Stop) | Err(_) => break,
            Ok(Msg::Dir { path, node, depth }) => {
                if sh.control.state() == ControlState::Cancelling {
                    sh.pending.fetch_sub(1, Ordering::Release);
                    break;
                }
                // パニックしても pending を取りこぼさず、ワーカーも継続する
                let guard = PendingGuard(&sh);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_dir(&sh, path, node, depth)
                }));
                drop(guard);
                if result.is_err() {
                    sh.record_error(
                        Path::new("<internal>"),
                        "内部エラー: 走査ワーカーが異常終了しました".into(),
                        None,
                    );
                }
            }
        }
    }
}

/// タスク完了時に pending を減算するガード (パニック時も Drop される)。
struct PendingGuard<'a>(&'a Shared);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::Release);
    }
}

fn is_excluded(excludes: &[String], name: &OsString) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let s = name.to_string_lossy();
    excludes.iter().any(|pat| path::glob_match(pat, &s))
}

fn process_dir(sh: &Arc<Shared>, path: PathBuf, node: u32, depth: u16) {
    {
        let mut cur = lock(&sh.progress.current);
        *cur = path.to_string_lossy().into_owned();
    }
    sh.progress.dirs.fetch_add(1, Ordering::Relaxed);

    sh.control.wait_if_paused();
    if sh.control.state() == ControlState::Cancelling {
        return;
    }

    let rd = match fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(e) => {
            sh.record_error(&path, e.to_string(), Some(node));
            return;
        }
    };

    let mut batch: Vec<Node> = Vec::new();

    for entry in rd.flatten() {
        if sh.control.state() == ControlState::Cancelling {
            break;
        }
        if sh.control.state() == ControlState::Paused {
            sh.control.wait_if_paused();
            if sh.control.state() == ControlState::Cancelling {
                break;
            }
        }
        sh.pacer.acquire();
        sh.progress.entries.fetch_add(1, Ordering::Relaxed);

        let name = entry.file_name();
        if !sh.cfg.include_hidden && platform::is_hidden_name(&name) {
            continue;
        }
        if is_excluded(&sh.cfg.excludes, &name) {
            continue;
        }
        let child_path = path.join(&name);
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                sh.record_error(&child_path, e.to_string(), Some(node));
                continue;
            }
        };

        // max_depth を超えるエントリ (ファイル・ディレクトリ・リンク) は作らない
        if sh.cfg.max_depth.is_some_and(|m| depth + 1 > m) {
            sh.mark_truncated(node);
            continue;
        }

        let md = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                sh.record_error(&child_path, e.to_string(), Some(node));
                continue;
            }
        };
        if !sh.cfg.include_hidden && platform::has_hidden_or_system_attribute(&md) {
            continue;
        }
        let is_reparse = platform::is_reparse_point(&md);

        if ft.is_dir() && !ft.is_symlink() && !is_reparse {
            if sh.cfg.skip_pseudo && path::is_pseudo_path(&child_path) {
                continue;
            }
            let volume = if sh.wants_volume(false) {
                platform::volume_of(&child_path)
            } else {
                None
            };
            if !sh.dir_allowed(&child_path, volume, false) {
                batch.push(Node::blocked(
                    name,
                    node,
                    depth + 1,
                    platform::mtime_secs(&md),
                ));
                continue;
            }
            if sh.cfg.follow_links && !sh.mark_visited(&md, &child_path) {
                batch.push(Node::other(
                    name,
                    node,
                    depth + 1,
                    0,
                    md.len(),
                    platform::mtime_secs(&md),
                ));
                continue;
            }
            if !sh.queue_dir(node, name, child_path, depth + 1, platform::mtime_secs(&md)) {
                break; // ノード上限、またはワーカー停止
            }
            continue;
        }

        if ft.is_file() {
            sh.add_file(&mut batch, name, node, depth + 1, &md, &child_path);
            continue;
        }

        // シンボリックリンク / 再解析ポイント (ジャンクション等) / 特殊ファイル
        // 既定では辿らない。辿るのは --follow-links 指定時のみ。
        let leaf = || {
            Node::other(
                name.clone(),
                node,
                depth + 1,
                0,
                md.len(),
                platform::mtime_secs(&md),
            )
        };
        if (ft.is_symlink() || is_reparse) && sh.cfg.follow_links {
            // 厳格モード (-x / --local-only) ではリンクを一切辿らない。
            // リンク先のメタデータ取得自体が別FS (ネットワーク等) への
            // アクセスになり得るため。
            if sh.cfg.local_only || sh.cfg.one_file_system {
                batch.push(leaf());
                continue;
            }
            // Windows: リンク先が別ドライブ/UNC なら辿らない (read_link は外部に触れない)
            #[cfg(windows)]
            if platform::link_leaves_volume(&child_path, sh.root_drive) {
                batch.push(leaf());
                continue;
            }
            let volume = if sh.wants_volume(true) {
                platform::volume_of(&child_path)
            } else {
                None
            };
            if sh.link_cross_blocked(volume) {
                batch.push(leaf());
                continue;
            }
            match fs::metadata(&child_path) {
                Ok(tmd) if tmd.is_dir() => {
                    if !sh.dir_allowed(&child_path, volume, true) {
                        batch.push(leaf());
                        continue;
                    }
                    if !sh.mark_visited(&tmd, &child_path) {
                        batch.push(leaf());
                        continue;
                    }
                    if !sh.queue_dir(
                        node,
                        name,
                        child_path,
                        depth + 1,
                        platform::mtime_secs(&tmd),
                    ) {
                        break;
                    }
                }
                Ok(tmd) => {
                    // リンク先がファイルの場合も直接走査と同じフィルタ
                    // (min_size / クラウドプレースホルダ) を適用する。
                    sh.add_file(&mut batch, name, node, depth + 1, &tmd, &child_path);
                }
                Err(e) => {
                    sh.record_error(&child_path, e.to_string(), Some(node));
                }
            }
        } else {
            batch.push(leaf());
        }
    }

    if !batch.is_empty() {
        let mut t = lock(&sh.tree);
        for n in batch {
            if sh.push_locked(&mut t, node, n).is_none() {
                break;
            }
        }
    }
}

impl Shared {
    /// ボリュームIDの取得が必要か (余計な stat を避ける)
    fn wants_volume(&self, is_link: bool) -> bool {
        self.cfg.one_file_system
            || self.cfg.local_only
            || (is_link && (self.cfg.network || cfg!(windows)))
    }

    /// このディレクトリへ入ってよいか。
    ///
    /// - `--local-only`: ネットワークFS (Unix) / ルートと異なるボリューム (Windows) を除外
    /// - `-x`: ルートと同一ボリュームのみ
    /// - リンク追従: `-x` / `--local-only` / network / Windows では越境しない
    fn dir_allowed(&self, child: &Path, volume: Option<u64>, is_link: bool) -> bool {
        #[cfg(windows)]
        let _ = child;
        if self.cfg.local_only {
            #[cfg(windows)]
            if volume != Some(self.root_volume) {
                return false;
            }
            #[cfg(not(windows))]
            {
                let vol = volume.unwrap_or(0);
                let cached = lock(&self.netfs_cache).get(&vol).copied();
                let net = match cached {
                    Some(v) => v,
                    None => {
                        let n = platform::is_network_fs(child);
                        lock(&self.netfs_cache).insert(vol, n);
                        n
                    }
                };
                if net {
                    return false;
                }
            }
        }
        if self.cfg.one_file_system && volume != Some(self.root_volume) {
            return false;
        }
        if is_link {
            let restrict = self.cfg.one_file_system
                || self.cfg.local_only
                || self.cfg.network
                || cfg!(windows);
            if restrict && volume != Some(self.root_volume) {
                return false;
            }
        }
        true
    }

    /// リンク先がルートのボリュームを越えるため辿らない設定か。
    fn link_cross_blocked(&self, volume: Option<u64>) -> bool {
        let restrict =
            self.cfg.one_file_system || self.cfg.local_only || self.cfg.network || cfg!(windows);
        restrict && volume != Some(self.root_volume)
    }

    /// リンク追従時の重複・循環検出。初出なら true。
    fn mark_visited(&self, md: &Metadata, path: &Path) -> bool {
        match platform::dir_key(md, path) {
            Some(k) => lock(&self.visited).insert(k),
            None => true,
        }
    }

    /// ハードリンク判定が必要か。Windows のネットワークプロファイルでは
    /// 追加のネットワーク往復 (ハンドルオープン) を避けるため省略する。
    fn want_hardlink(&self) -> bool {
        !(self.cfg.network && cfg!(windows))
    }

    /// ファイルノードをバッチへ追加する。min_size とクラウドプレースホルダ
    /// 除外を適用し、ハードリンクの重複計上もここで処理する。
    /// 追加した場合は true を返す。
    fn add_file(
        &self,
        batch: &mut Vec<Node>,
        name: OsString,
        parent: u32,
        depth: u16,
        md: &Metadata,
        path: &Path,
    ) -> bool {
        let logical = md.len();
        if logical < self.cfg.min_size {
            return false;
        }
        if self.cfg.skip_cloud_placeholders && platform::is_cloud_placeholder(md) {
            return false;
        }
        // 割当サイズとハードリンクキー (Windows では 1 回のハンドルオープンで両方)。
        let info =
            platform::file_size_info(md, path, self.cfg.want_allocated, self.want_hardlink());
        let own_alloc = match info.hardlink {
            Some(key) => {
                let mut m = lock(&self.hardlinks);
                if let std::collections::hash_map::Entry::Vacant(e) = m.entry(key) {
                    e.insert(());
                    info.alloc
                } else {
                    0 // 既に計上済みの実体
                }
            }
            None => info.alloc,
        };
        batch.push(Node::file(
            name,
            parent,
            depth,
            own_alloc,
            logical,
            platform::mtime_secs(md),
        ));
        self.progress
            .bytes_alloc
            .fetch_add(own_alloc, Ordering::Relaxed);
        self.progress
            .bytes_logical
            .fetch_add(logical, Ordering::Relaxed);
        true
    }

    /// ディレクトリノードを登録して走査タスクを投入する。
    /// ノード上限に達した、またはワーカーが停止した場合は false。
    fn queue_dir(
        &self,
        parent: u32,
        name: OsString,
        path: PathBuf,
        depth: u16,
        mtime: i64,
    ) -> bool {
        let idx = {
            let mut t = lock(&self.tree);
            match self.push_locked(&mut t, parent, Node::dir(name, parent, depth, mtime)) {
                Some(i) => i,
                None => return false,
            }
        };
        self.pending.fetch_add(1, Ordering::Release);
        if self
            .tx
            .send(Msg::Dir {
                path,
                node: idx,
                depth,
            })
            .is_err()
        {
            self.pending.fetch_sub(1, Ordering::Release);
            return false;
        }
        true
    }

    fn push_locked(&self, t: &mut Tree, parent: u32, n: Node) -> Option<u32> {
        let max = if self.cfg.max_nodes == 0 {
            u64::MAX
        } else {
            self.cfg.max_nodes
        };
        t.push_child_capped(parent, n, max)
    }

    fn mark_truncated(&self, node: u32) {
        self.truncated.store(true, Ordering::Relaxed);
        let mut t = lock(&self.tree);
        t.nodes[node as usize].truncated = true;
    }

    fn record_error(&self, path: &Path, message: String, node: Option<u32>) {
        self.progress.errors.fetch_add(1, Ordering::Relaxed);
        {
            let mut v = lock(&self.errors);
            if v.len() < MAX_RECORDED_ERRORS {
                v.push(ScanError {
                    path: path.to_string_lossy().into_owned(),
                    message,
                });
            }
        }
        if let Some(n) = node {
            let mut t = lock(&self.tree);
            t.nodes[n as usize].err_count += 1;
        }
    }
}
