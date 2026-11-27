//! 並列スキャンエンジン。カスタムワーカープールにより、一時停止・キャンセル、
//! レート制限 (エントリ/stat 処理数ベース) を確実に制御する。

#[cfg(unix)]
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Sender};

use super::node::{FileId, Node, Tree};
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

/// テスト専用: 指定ディレクトリの ReadDir を途中で失敗させる。
#[cfg(test)]
mod readdir_fault {
    use super::*;

    static FAULT: Mutex<Option<(PathBuf, usize)>> = Mutex::new(None);

    pub(super) fn arm(path: &Path, after: usize) {
        *lock(&FAULT) = Some((path.to_path_buf(), after));
    }

    pub(super) fn should_fail(path: &Path) -> bool {
        let mut guard = lock(&FAULT);
        let Some((target, remaining)) = guard.as_mut() else {
            return false;
        };
        if target != path {
            return false;
        }
        if *remaining == 0 {
            *guard = None;
            return true;
        }
        *remaining -= 1;
        false
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

/// テスト用のファイルシステム判定モック。
///
/// 実際の NFS/SMB マウントを CI で用意できないため、パスごとの
/// ボリュームIDと分類を差し替える。
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct FsMock {
    pub volume: fn(&Path) -> Option<u64>,
    pub info: fn(&Path) -> platform::FsInfo,
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
    /// 削除時の同一性検証用のファイル ID を収集する (`--allow-delete` 時のみ)。
    pub want_identity: bool,
    pub skip_pseudo: bool,
    /// ネットワークプロファイル (UNC / マウントされたネットワークFS)。
    /// Windows では追加のネットワーク往復を避けるためハードリンク判定を省略し、
    /// リンク追従は同一ボリュームに制限する。
    pub network: bool,
    /// ルートが Windows の UNC / ネットワークドライブか。
    /// `--profile local` を明示してもネットワーク上の ID 収集を避ける。
    pub root_is_network: bool,
    /// --local-only: ネットワークFSへの走査を一切行わない。
    pub local_only: bool,
    /// テスト用のファイルシステム判定モック。
    #[cfg(test)]
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fs_mock: Option<FsMock>,
}

impl EngineConfig {
    /// 削除時の同一性検証用 ID を収集するか。
    ///
    /// ネットワークプロファイルでは、追加のネットワーク往復とサーバ依存の
    /// 不安定な ID を避けるため収集しない。Windows の UNC / ネットワーク
    /// ドライブは `--profile local` 指定時も同様に収集しない。
    pub fn collect_identity(&self) -> bool {
        self.want_identity && self.identity_blocked_reason().is_none()
    }

    /// ネットワークのために ID 収集を停止している理由。
    pub fn identity_blocked_reason(&self) -> Option<&'static str> {
        if self.network {
            Some("ネットワークプロファイルではファイル ID を収集しないため削除できません")
        } else if self.root_is_network {
            Some("ネットワーク上のパスではファイル ID を収集しないため削除できません")
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub struct ScanError {
    pub path: String,
    pub message: String,
}

pub struct ScanOutcome {
    pub tree: Tree,
    pub errors: Vec<ScanError>,
    /// 2 本以上のパスが存在するハードリンクのグループ (node index)。
    /// 削除時に allocated の計上先を残存リンクへ移すために使う。
    pub hardlink_groups: HashMap<(u64, u64), Vec<u32>>,
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

type PendingNode = (Node, Option<(u64, u64)>, Option<NodeIdentity>);

#[derive(Clone, Copy)]
struct NodeIdentity {
    id: FileId,
    link_like: bool,
}

/// ディレクトリ配下で削除用 ID を収集してよいかのスコープ。
#[derive(Clone, Copy)]
struct IdentityScope {
    allowed: bool,
    /// Unix の st_dev。マウントポイントの一意な識別子ではなく、
    /// ファイルシステム分類キャッシュのキーとしてのみ使う。
    volume: Option<u64>,
}

enum Msg {
    Dir {
        path: PathBuf,
        node: u32,
        depth: u16,
        identity: IdentityScope,
    },
    Stop,
}

struct Shared {
    cfg: EngineConfig,
    tx: Sender<Msg>,
    pending: AtomicU64,
    tree: Arc<Mutex<Tree>>,
    hardlinks: Mutex<HashMap<(u64, u64), ()>>,
    /// 実際に重複するハードリンクの node 一覧 (スキャン完了後に確定)。
    hardlink_groups: Mutex<HashMap<(u64, u64), Vec<u32>>>,
    visited: Mutex<HashSet<(u64, u64)>>,
    progress: Arc<Progress>,
    control: Arc<Control>,
    pacer: Pacer,
    errors: Arc<Mutex<Vec<ScanError>>>,
    root_volume: Option<u64>,
    /// ルートのドライブレター (Windows のリンク越境判定用)
    #[cfg_attr(not(windows), allow(dead_code))]
    root_drive: Option<u8>,
    truncated: AtomicBool,
    /// st_dev → ファイルシステム分類。
    /// `--local-only` と削除用 ID 収集で共有する。
    #[cfg_attr(windows, allow(dead_code))]
    fs_class_cache: Mutex<HashMap<u64, platform::FsInfo>>,
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

/// ルートと対象が同一ボリュームか。どちらかが不明なら一致とみなさない。
/// 不明値を 0 などの実在し得る値に読み替えないためのヘルパ。
fn same_volume(root: Option<u64>, volume: Option<u64>) -> bool {
    match (root, volume) {
        (Some(a), Some(b)) => a == b,
        _ => false,
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
    let root_volume = platform::volume_of(&fs_root);

    let initial_tree = Tree::with_root(cfg.root.as_os_str().to_owned(), root_mtime);
    let tree_arc = Arc::new(Mutex::new(initial_tree));
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
        hardlink_groups: Mutex::new(HashMap::new()),
        visited: Mutex::new(visited),
        progress: progress.clone(),
        control: control.clone(),
        pacer: Pacer::new(cfg.rate_limit),
        errors: errors_arc,
        root_volume,
        root_drive: root_drive_of(&fs_root),
        truncated: AtomicBool::new(false),
        fs_class_cache: Mutex::new(HashMap::new()),
        cfg,
    });

    let root_identity = shared.root_identity_scope(&fs_root, root_md.as_ref());
    if root_identity.allowed {
        if let Some(md) = &root_md {
            if let Some(id) = platform::file_id(md, &fs_root, false) {
                lock(&shared.tree).set_id(root, id, false);
            }
        }
    }

    if shared.root_volume.is_none() && (shared.cfg.one_file_system || shared.cfg.local_only) {
        shared.record_error(
            Path::new("<volume>"),
            "ルートのボリュームを取得できないため、-x / --local-only の境界判定を適用できません (走査はルートのみ)".into(),
            None,
        );
    }

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
            identity: root_identity,
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
    let mut hardlink_groups = shared
        .hardlink_groups
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    // 走査範囲内で実際に 2 本以上見つかったものだけを残す。
    hardlink_groups.retain(|_, nodes| nodes.len() > 1);
    assign_hardlink_representatives(&mut tree, &mut hardlink_groups);
    tree.aggregate();
    // max_depth だけでなく、max_nodes による打ち切りもノード単位で立つ。
    // 全体レポートの truncated に反映する。
    let tree_truncated = tree.nodes.iter().any(|n| n.truncated);
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
        hardlink_groups,
        truncated: shared.truncated.load(Ordering::Relaxed) || tree_truncated,
        cancelled,
        root: shared.cfg.root.clone(),
    }
}

/// ハードリンクグループの allocated を、ルートからのパス順で最小のノードへ
/// 集約する。並列走査の順序に依存せず、フォルダ別集計を再現可能にする。
fn assign_hardlink_representatives(tree: &mut Tree, groups: &mut HashMap<(u64, u64), Vec<u32>>) {
    for nodes in groups.values_mut() {
        let mut keyed: Vec<(PathBuf, u32)> = nodes
            .iter()
            .map(|&idx| (node_path(tree, idx), idx))
            .collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        nodes.clear();
        nodes.extend(keyed.iter().map(|&(_, idx)| idx));

        let alloc = nodes
            .iter()
            .map(|&idx| tree.nodes[idx as usize].own_alloc)
            .sum::<u64>();
        for &idx in nodes.iter() {
            tree.nodes[idx as usize].own_alloc = 0;
        }
        if let Some(&rep) = nodes.first() {
            tree.nodes[rep as usize].own_alloc = alloc;
        }
    }
}

fn node_path(tree: &Tree, idx: u32) -> PathBuf {
    let mut names = Vec::new();
    let mut cur = idx;
    while cur != tree.root {
        let n = tree.node(cur);
        if n.parent == u32::MAX {
            break;
        }
        names.push(n.name.clone());
        cur = n.parent;
    }
    let mut path = PathBuf::from(tree.node(tree.root).name.as_ref());
    for name in names.iter().rev() {
        path.push(name.as_ref());
    }
    path
}

fn worker(sh: Arc<Shared>, rx: crossbeam_channel::Receiver<Msg>) {
    loop {
        if sh.control.state() == ControlState::Cancelling {
            break;
        }
        match rx.recv() {
            Ok(Msg::Stop) | Err(_) => break,
            Ok(Msg::Dir {
                path,
                node,
                depth,
                identity,
            }) => {
                if sh.control.state() == ControlState::Cancelling {
                    sh.pending.fetch_sub(1, Ordering::Release);
                    break;
                }
                // パニックしても pending を取りこぼさず、ワーカーも継続する
                let guard = PendingGuard(&sh);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_dir(&sh, path, node, depth, identity)
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

fn process_dir(
    sh: &Arc<Shared>,
    path: PathBuf,
    node: u32,
    depth: u16,
    parent_identity: IdentityScope,
) {
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

    let mut batch: Vec<PendingNode> = Vec::new();

    for entry_result in rd {
        if sh.control.state() == ControlState::Cancelling {
            break;
        }
        if sh.control.state() == ControlState::Paused {
            sh.control.wait_if_paused();
            if sh.control.state() == ControlState::Cancelling {
                break;
            }
        }
        #[cfg(test)]
        let entry_result = if readdir_fault::should_fail(&path) {
            Err(std::io::Error::other("injected readdir error"))
        } else {
            entry_result
        };
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(e) => {
                // エントリ名は得られないため、列挙に失敗したディレクトリを記録する。
                // このエラーを無視すると、取得できなかった項目が無通知で欠落する。
                sh.record_error(
                    &path,
                    format!("ディレクトリ列挙中にエラー: {e}"),
                    Some(node),
                );
                continue;
            }
        };
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
            let child_identity = sh.identity_scope(&child_path, &md, parent_identity);
            let volume = child_identity.volume.or_else(|| {
                if sh.wants_volume(false) {
                    platform::volume_of(&child_path)
                } else {
                    None
                }
            });
            if !sh.dir_allowed(&child_path, volume, false) {
                let id = sh.identity(&md, &child_path, false, child_identity.allowed);
                batch.push((
                    Node::blocked(name, node, depth + 1, platform::mtime_secs(&md)),
                    None,
                    id,
                ));
                continue;
            }
            if sh.cfg.follow_links && !sh.mark_visited(&md, &child_path) {
                let id = sh.identity(&md, &child_path, false, child_identity.allowed);
                batch.push((
                    Node::other(
                        name,
                        node,
                        depth + 1,
                        0,
                        md.len(),
                        platform::mtime_secs(&md),
                    ),
                    None,
                    id,
                ));
                continue;
            }
            let id = sh.identity(&md, &child_path, false, child_identity.allowed);
            if !sh.queue_dir(
                node,
                name,
                child_path,
                depth + 1,
                platform::mtime_secs(&md),
                id,
                child_identity,
            ) {
                break; // ノード上限、またはワーカー停止
            }
            continue;
        }

        if ft.is_file() {
            let child_identity = sh.identity_scope(&child_path, &md, parent_identity);
            sh.add_file(
                &mut batch,
                name,
                node,
                depth + 1,
                &md,
                &child_path,
                None,
                child_identity.allowed,
            );
            continue;
        }

        // シンボリックリンク / 再解析ポイント (ジャンクション等) / 特殊ファイル
        // 既定では辿らない。辿るのは --follow-links 指定時のみ。
        // リンク自体は、リンクが存在する親ディレクトリのスコープで判定する。
        let leaf_id = sh.identity(
            &md,
            &child_path,
            ft.is_symlink() || is_reparse,
            parent_identity.allowed,
        );
        let leaf = || {
            (
                Node::other(
                    name.clone(),
                    node,
                    depth + 1,
                    0,
                    md.len(),
                    platform::mtime_secs(&md),
                ),
                None,
                leaf_id,
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
                    // リンク先は独立したファイルシステムとして分類する。
                    // リンク元の親ディレクトリの分類を継承しない。
                    let target_identity = sh.identity_scope(&child_path, &tmd, parent_identity);
                    let link_id = sh.identity(&md, &child_path, true, parent_identity.allowed);
                    if !sh.queue_dir(
                        node,
                        name,
                        child_path,
                        depth + 1,
                        platform::mtime_secs(&tmd),
                        link_id,
                        target_identity,
                    ) {
                        break;
                    }
                }
                Ok(tmd) => {
                    // リンク先がファイルの場合も直接走査と同じフィルタ
                    // (min_size / クラウドプレースホルダ) を適用する。
                    // サイズはリンク先、削除時の ID はリンク自身を使う。
                    let target_identity = sh.identity_scope(&child_path, &tmd, parent_identity);
                    let link_id = sh.identity(&md, &child_path, true, parent_identity.allowed);
                    sh.add_file(
                        &mut batch,
                        name,
                        node,
                        depth + 1,
                        &tmd,
                        &child_path,
                        link_id,
                        target_identity.allowed,
                    );
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
        let mut links: Vec<(u32, (u64, u64))> = Vec::new();
        {
            let mut t = lock(&sh.tree);
            for (n, hardlink, identity) in batch {
                match sh.push_locked(&mut t, node, n) {
                    Some(idx) => {
                        if sh.cfg.collect_identity() {
                            if let Some(identity) = identity {
                                if identity.id.is_known() {
                                    t.set_id(idx, identity.id, identity.link_like);
                                }
                            }
                        }
                        if let Some(key) = hardlink {
                            links.push((idx, key));
                        }
                    }
                    None => break,
                }
            }
        }
        if !links.is_empty() {
            let mut groups = lock(&sh.hardlink_groups);
            for (idx, key) in links {
                groups.entry(key).or_default().push(idx);
            }
        }
    }
}

/// 分類結果をキャッシュへ first-wins で格納する。
///
/// 同じ st_dev に対して複数ワーカーが同時に分類しても、先に格納された
/// `FsInfo` (class と local_only_network の組) を以後の全員が使う。
#[cfg(unix)]
fn store_fs_info(
    cache: &Mutex<HashMap<u64, platform::FsInfo>>,
    volume: u64,
    info: platform::FsInfo,
) -> platform::FsInfo {
    let mut cache = lock(cache);
    match cache.entry(volume) {
        Entry::Occupied(entry) => *entry.get(),
        Entry::Vacant(entry) => {
            entry.insert(info);
            info
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

    /// メタデータから得たボリュームID。テストではモックで差し替える。
    #[cfg(unix)]
    fn fs_volume(&self, path: &Path, md: &Metadata) -> Option<u64> {
        #[cfg(test)]
        if let Some(mock) = self.cfg.fs_mock {
            return (mock.volume)(path);
        }
        let _ = path;
        platform::metadata_volume(md)
    }

    /// ボリューム単位でファイルシステム分類をキャッシュする。
    ///
    /// st_dev はマウントポイントの一意な識別子ではないが、同じ st_dev は
    /// 同じファイルシステム実体を指すため、bind mount を含めて分類を共有できる。
    #[cfg(unix)]
    fn fs_info(&self, path: &Path, volume: Option<u64>) -> platform::FsInfo {
        if let Some(vol) = volume {
            if let Some(info) = lock(&self.fs_class_cache).get(&vol).copied() {
                return info;
            }
        }
        // 分類はロック外で行う。結果の格納は first-wins。
        let info = self.classify_fs_path(path);
        if let Some(vol) = volume {
            store_fs_info(&self.fs_class_cache, vol, info)
        } else {
            info
        }
    }

    #[cfg(unix)]
    fn classify_fs_path(&self, path: &Path) -> platform::FsInfo {
        #[cfg(test)]
        if let Some(mock) = self.cfg.fs_mock {
            return (mock.info)(path);
        }
        platform::classify_fs(path)
    }

    /// 子ノードの ID 収集スコープを決める。
    ///
    /// - 同じ st_dev なら親の許可状態を継承
    /// - 異なる st_dev なら分類し、Local のときだけ許可
    /// - 親が不許可なら分類せず不許可を継承
    #[cfg(unix)]
    fn identity_scope(&self, path: &Path, md: &Metadata, parent: IdentityScope) -> IdentityScope {
        let volume = self.fs_volume(path, md);
        if !self.cfg.collect_identity() || !parent.allowed {
            return IdentityScope {
                allowed: false,
                volume,
            };
        }
        let allowed = match (parent.volume, volume) {
            (Some(a), Some(b)) if a == b => true,
            _ => self.fs_info(path, volume).class == platform::FsClass::Local,
        };
        IdentityScope { allowed, volume }
    }

    #[cfg(not(unix))]
    fn identity_scope(&self, _path: &Path, _md: &Metadata, parent: IdentityScope) -> IdentityScope {
        IdentityScope {
            allowed: parent.allowed && self.cfg.collect_identity(),
            volume: None,
        }
    }

    /// ルートのスコープ。ルートの分類に失敗した場合は安全側で不許可。
    #[cfg(unix)]
    fn root_identity_scope(&self, path: &Path, md: Option<&Metadata>) -> IdentityScope {
        let volume = md.and_then(|m| self.fs_volume(path, m));
        if !self.cfg.collect_identity() || md.is_none() {
            return IdentityScope {
                allowed: false,
                volume,
            };
        }
        let info = self.fs_info(path, volume);
        IdentityScope {
            allowed: info.class == platform::FsClass::Local,
            volume,
        }
    }

    #[cfg(not(unix))]
    fn root_identity_scope(&self, _path: &Path, _md: Option<&Metadata>) -> IdentityScope {
        IdentityScope {
            allowed: self.cfg.collect_identity(),
            volume: None,
        }
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
            if !same_volume(self.root_volume, volume) {
                return false;
            }
            #[cfg(not(windows))]
            {
                // 既存の --local-only 挙動を維持するため、ID 分類とは別の
                // local_only_network を使う。
                if self.fs_info(child, volume).local_only_network {
                    return false;
                }
            }
        }
        if self.cfg.one_file_system && !same_volume(self.root_volume, volume) {
            return false;
        }
        if is_link {
            let restrict = self.cfg.one_file_system
                || self.cfg.local_only
                || self.cfg.network
                || cfg!(windows);
            if restrict && !same_volume(self.root_volume, volume) {
                return false;
            }
        }
        true
    }

    /// リンク先がルートのボリュームを越えるため辿らない設定か。
    fn link_cross_blocked(&self, volume: Option<u64>) -> bool {
        let restrict =
            self.cfg.one_file_system || self.cfg.local_only || self.cfg.network || cfg!(windows);
        restrict && !same_volume(self.root_volume, volume)
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

    /// 削除時の同一性検証用 ID。要求されていない、またはスコープ外なら None。
    fn identity(
        &self,
        md: &Metadata,
        path: &Path,
        link_like: bool,
        allowed: bool,
    ) -> Option<NodeIdentity> {
        if !self.cfg.collect_identity() || !allowed {
            return None;
        }
        platform::file_id(md, path, link_like).map(|id| NodeIdentity { id, link_like })
    }

    /// ファイルノードをバッチへ追加する。min_size とクラウドプレースホルダ
    /// 除外を適用し、ハードリンクの重複計上もここで処理する。
    /// 追加した場合は true を返す。
    #[allow(clippy::too_many_arguments)]
    fn add_file(
        &self,
        batch: &mut Vec<PendingNode>,
        name: OsString,
        parent: u32,
        depth: u16,
        md: &Metadata,
        path: &Path,
        id_override: Option<NodeIdentity>,
        identity_allowed: bool,
    ) -> bool {
        let logical = md.len();
        if logical < self.cfg.min_size {
            return false;
        }
        if self.cfg.skip_cloud_placeholders && platform::is_cloud_placeholder(md) {
            return false;
        }
        // 割当サイズとハードリンクキー (Windows では 1 回のハンドルオープンで両方)。
        // Windows ローカルでは表示が logical でも allocated を取得しておく。
        // そうしないと TUI の「ディスク使用量」切替や JSON の allocated が
        // 見かけのサイズになってしまう。ネットワークでは余計な往復を避ける。
        let collect_alloc = self.cfg.want_allocated || (cfg!(windows) && !self.cfg.network);
        // リンク自身の ID (id_override) は親スコープで決まるが、リンク先の
        // ファイル ID はリンク先のスコープでのみ収集する。
        let collect_own_identity = self.cfg.collect_identity() && identity_allowed;
        let info = platform::file_size_info(
            md,
            path,
            collect_alloc,
            self.want_hardlink(),
            collect_own_identity,
        );
        if collect_alloc && !info.alloc_exact {
            self.record_error(
                path,
                "割当サイズを取得できないため、見かけのサイズで代替しました".into(),
                Some(parent),
            );
        }
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
        let identity = if self.cfg.collect_identity() {
            id_override.or_else(|| {
                if collect_own_identity {
                    info.id.map(|id| NodeIdentity {
                        id,
                        link_like: false,
                    })
                } else {
                    None
                }
            })
        } else {
            None
        };
        batch.push((
            Node::file(
                name,
                parent,
                depth,
                own_alloc,
                logical,
                platform::mtime_secs(md),
            ),
            info.hardlink,
            identity,
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
    #[allow(clippy::too_many_arguments)]
    fn queue_dir(
        &self,
        parent: u32,
        name: OsString,
        path: PathBuf,
        depth: u16,
        mtime: i64,
        identity: Option<NodeIdentity>,
        scope: IdentityScope,
    ) -> bool {
        let idx = {
            let mut t = lock(&self.tree);
            match self.push_locked(&mut t, parent, Node::dir(name, parent, depth, mtime)) {
                Some(i) => {
                    // identity はリンク自身のスコープで既に判定済み。
                    // リンク先のスコープ (scope) が不許可でも、ローカル上の
                    // リンク自身の ID は保持できる。
                    if self.cfg.collect_identity() {
                        if let Some(identity) = identity {
                            if identity.id.is_known() {
                                t.set_id(i, identity.id, identity.link_like);
                            }
                        }
                    }
                    i
                }
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
                identity: scope,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::scan::node::NodeKind;
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
            threads: 1,
            rate_limit: 0,
            skip_cloud_placeholders: false,
            want_allocated: true,
            want_identity: false,
            skip_pseudo: false,
            network: false,
            root_is_network: false,
            local_only: false,
            fs_mock: None,
        }
    }

    #[test]
    fn volume_comparison_requires_known_values() {
        assert!(same_volume(Some(1), Some(1)));
        assert!(!same_volume(Some(1), Some(2)));
        assert!(!same_volume(None, Some(1)));
        assert!(!same_volume(Some(1), None));
        assert!(!same_volume(None, None));
    }

    #[cfg(unix)]
    #[test]
    fn fs_cache_first_classification_wins_for_same_volume() {
        let cache = Mutex::new(HashMap::new());
        let first = platform::FsInfo {
            class: platform::FsClass::Local,
            local_only_network: false,
        };
        let second = platform::FsInfo {
            class: platform::FsClass::Network,
            local_only_network: true,
        };

        assert_eq!(store_fs_info(&cache, 10, first), first);
        assert_eq!(store_fs_info(&cache, 10, second), first);
        assert_eq!(lock(&cache).get(&10).copied(), Some(first));
    }

    #[cfg(unix)]
    #[test]
    fn fs_cache_unknown_false_is_kept_first() {
        let cache = Mutex::new(HashMap::new());
        let failed = platform::FsInfo {
            class: platform::FsClass::Unknown,
            local_only_network: false,
        };
        let later = platform::FsInfo {
            class: platform::FsClass::Local,
            local_only_network: false,
        };

        assert_eq!(store_fs_info(&cache, 11, failed), failed);
        assert_eq!(store_fs_info(&cache, 11, later), failed);
        assert_eq!(lock(&cache).get(&11).copied(), Some(failed));
    }

    #[cfg(unix)]
    #[test]
    fn fs_cache_keeps_class_and_local_only_combination_intact() {
        let cache = Mutex::new(HashMap::new());
        // 実装上は Local + true は通常現れないが、キャッシュが class と
        // local_only_network を別々に混ぜないことを確認する。
        let first = platform::FsInfo {
            class: platform::FsClass::Local,
            local_only_network: true,
        };
        let second = platform::FsInfo {
            class: platform::FsClass::Network,
            local_only_network: false,
        };

        assert_eq!(store_fs_info(&cache, 12, first), first);
        assert_eq!(store_fs_info(&cache, 12, second), first);
        assert_eq!(lock(&cache).get(&12).copied(), Some(first));
    }

    #[cfg(unix)]
    #[test]
    fn same_volume_uses_first_fs_info_for_different_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("samevol-a")).unwrap();
        fs::create_dir_all(root.join("samevol-b")).unwrap();
        fs::write(root.join("samevol-a/a.txt"), b"a").unwrap();
        fs::write(root.join("samevol-b/b.txt"), b"b").unwrap();

        let mut c = cfg(&root);
        c.want_identity = true;
        c.threads = 1;
        c.fs_mock = mock_fs();
        let out = run(c, Arc::new(Progress::new()), Arc::new(Control::new()));

        let a = find_node(&out, &["samevol-a"]).unwrap();
        let b = find_node(&out, &["samevol-b"]).unwrap();
        assert_eq!(
            out.tree.id(a).is_known(),
            out.tree.id(b).is_known(),
            "same st_dev must use the same first FsInfo"
        );
        let af = find_node(&out, &["samevol-a", "a.txt"]).unwrap();
        let bf = find_node(&out, &["samevol-b", "b.txt"]).unwrap();
        assert_eq!(
            out.tree.id(af).is_known(),
            out.tree.id(bf).is_known(),
            "descendants must inherit the same FsInfo"
        );
    }

    #[cfg(unix)]
    fn path_has_prefix(path: &Path, prefix: &str) -> bool {
        path.components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with(prefix))
    }

    #[cfg(unix)]
    fn mock_fs_info(path: &Path) -> platform::FsInfo {
        if path_has_prefix(path, "recover") {
            // ネットワーク配下にあるローカルマウント。親が不許可なので
            // 子孫は再許可されないことを確認する。
            platform::FsInfo {
                class: platform::FsClass::Local,
                local_only_network: false,
            }
        } else if path_has_prefix(path, "net") || path_has_prefix(path, "to_net") {
            platform::FsInfo {
                class: platform::FsClass::Network,
                local_only_network: true,
            }
        } else if path_has_prefix(path, "fuse") {
            platform::FsInfo {
                class: platform::FsClass::Unknown,
                local_only_network: true,
            }
        } else if path_has_prefix(path, "unknown") {
            platform::FsInfo {
                class: platform::FsClass::Unknown,
                local_only_network: false,
            }
        } else if path_has_prefix(path, "samevol-a") {
            platform::FsInfo {
                class: platform::FsClass::Local,
                local_only_network: false,
            }
        } else if path_has_prefix(path, "samevol-b") {
            platform::FsInfo {
                class: platform::FsClass::Network,
                local_only_network: true,
            }
        } else {
            platform::FsInfo {
                class: platform::FsClass::Local,
                local_only_network: false,
            }
        }
    }

    #[cfg(unix)]
    fn mock_fs_volume(path: &Path) -> Option<u64> {
        if path_has_prefix(path, "recover") || path_has_prefix(path, "localmount") {
            Some(3)
        } else if path_has_prefix(path, "net") || path_has_prefix(path, "to_net") {
            Some(2)
        } else if path_has_prefix(path, "fuse") {
            Some(5)
        } else if path_has_prefix(path, "unknown") {
            Some(4)
        } else if path_has_prefix(path, "samevol-a") || path_has_prefix(path, "samevol-b") {
            Some(6)
        } else {
            Some(1)
        }
    }

    #[cfg(unix)]
    fn mock_fs() -> Option<FsMock> {
        Some(FsMock {
            volume: mock_fs_volume,
            info: mock_fs_info,
        })
    }

    #[cfg(unix)]
    fn find_node(out: &ScanOutcome, names: &[&str]) -> Option<u32> {
        let mut cur = out.tree.root;
        for name in names {
            cur = out
                .tree
                .node(cur)
                .children
                .iter()
                .copied()
                .find(|&i| out.tree.node(i).name.to_string_lossy() == *name)?;
        }
        Some(cur)
    }

    #[cfg(unix)]
    fn assert_unknown(out: &ScanOutcome, names: &[&str]) {
        let idx = find_node(out, names).unwrap_or_else(|| panic!("node {names:?}"));
        assert_eq!(
            out.tree.id(idx),
            FileId::UNKNOWN,
            "ID must be UNKNOWN: {names:?}"
        );
    }

    #[cfg(unix)]
    fn assert_known(out: &ScanOutcome, names: &[&str]) {
        let idx = find_node(out, names).unwrap_or_else(|| panic!("node {names:?}"));
        assert!(out.tree.id(idx).is_known(), "ID must be known: {names:?}");
    }

    fn wait_until(mut f: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn pause_resume_and_cancel_are_observed() {
        let dir = tempdir().unwrap();
        for i in 0..500 {
            fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
        }

        let mut c = cfg(dir.path());
        c.threads = 1;
        c.rate_limit = 100; // 20 トークン / 200ms。500件では数秒かかる。
        let progress = Arc::new(Progress::new());
        let control = Arc::new(Control::new());
        let p = progress.clone();
        let ctl = control.clone();
        let handle = std::thread::spawn(move || run(c, p, ctl));

        assert!(
            wait_until(
                || progress.entries.load(Ordering::Relaxed) >= 10,
                Duration::from_secs(2)
            ),
            "走査が開始されませんでした"
        );

        control.pause();
        std::thread::sleep(Duration::from_millis(150));
        let paused = progress.entries.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(200));
        let after_pause = progress.entries.load(Ordering::Relaxed);
        assert!(
            after_pause <= paused + 2,
            "一時停止中に走査が進みました: {paused} -> {after_pause}"
        );

        control.resume();
        assert!(
            wait_until(
                || progress.entries.load(Ordering::Relaxed) > after_pause,
                Duration::from_secs(2)
            ),
            "再開後に走査が進みませんでした"
        );

        control.cancel();
        let out = handle.join().unwrap();
        assert!(out.cancelled, "中止が ScanOutcome に反映されていません");
    }

    #[test]
    fn readdir_iteration_error_is_recorded_and_scan_continues() {
        let dir = tempdir().unwrap();
        for i in 0..6 {
            fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
        }

        readdir_fault::arm(dir.path(), 2);
        let out = run(
            cfg(dir.path()),
            Arc::new(Progress::new()),
            Arc::new(Control::new()),
        );

        assert!(out.error_count >= 1, "列挙中エラーが記録されていません");
        assert!(
            out.errors
                .iter()
                .any(|e| e.path == dir.path().to_string_lossy()),
            "列挙に失敗したディレクトリが errors にありません: {:?}",
            out.errors
        );
        assert!(
            out.tree.node(out.tree.root).file_count < 6,
            "エラー後のエントリまで数えられています"
        );
        assert!(
            out.tree.node(out.tree.root).file_count >= 1,
            "エラー後も走査を継続していません"
        );
    }

    #[cfg(unix)]
    #[test]
    fn nested_network_and_unknown_suppress_identity_and_delete() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("local.txt"), b"local").unwrap();
        fs::create_dir_all(root.join("localmount")).unwrap();
        fs::write(root.join("localmount/local.txt"), b"local").unwrap();
        fs::create_dir_all(root.join("net/deep")).unwrap();
        fs::write(root.join("net/net.txt"), b"net").unwrap();
        fs::write(root.join("net/deep/deep.txt"), b"net").unwrap();
        fs::create_dir_all(root.join("net/recover")).unwrap();
        fs::write(root.join("net/recover/recover.txt"), b"local").unwrap();
        fs::create_dir_all(root.join("unknown")).unwrap();
        fs::write(root.join("unknown/u.txt"), b"unknown").unwrap();
        fs::create_dir_all(root.join("fuse")).unwrap();
        fs::write(root.join("fuse/f.txt"), b"fuse").unwrap();
        symlink(root.join("local.txt"), root.join("net/lnk")).unwrap();

        let mut c = cfg(&root);
        c.want_identity = true;
        c.threads = 2;
        c.fs_mock = mock_fs();
        let out = run(c, Arc::new(Progress::new()), Arc::new(Control::new()));

        assert!(out.tree.id(0).is_known(), "local root ID");
        assert_known(&out, &["local.txt"]);
        assert_known(&out, &["localmount"]);
        assert_known(&out, &["localmount", "local.txt"]);

        for names in [
            &["net"][..],
            &["net", "net.txt"],
            &["net", "deep"],
            &["net", "deep", "deep.txt"],
            &["net", "recover"],
            &["net", "recover", "recover.txt"],
            &["unknown"],
            &["unknown", "u.txt"],
            &["fuse"],
            &["fuse", "f.txt"],
            &["net", "lnk"],
        ] {
            assert_unknown(&out, names);
        }

        // ネットワーク / Unknown 上の削除は拒否し、ローカルは成功する。
        for (rel, names) in [
            ("net/net.txt", &["net", "net.txt"][..]),
            ("unknown/u.txt", &["unknown", "u.txt"][..]),
            ("fuse/f.txt", &["fuse", "f.txt"][..]),
        ] {
            let idx = find_node(&out, names).unwrap();
            let path = root.join(rel);
            let err = crate::delete::remove_target(
                &path,
                out.tree.node(idx).kind,
                out.tree.id(idx),
                out.tree.id_link_like(idx),
            )
            .unwrap_err();
            assert!(err.contains("走査時"), "{rel}: {err}");
            assert!(path.exists(), "{rel}");
        }

        let local_idx = find_node(&out, &["local.txt"]).unwrap();
        crate::delete::remove_target(
            &root.join("local.txt"),
            out.tree.node(local_idx).kind,
            out.tree.id(local_idx),
            out.tree.id_link_like(local_idx),
        )
        .unwrap();
        assert!(!root.join("local.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn root_network_or_unknown_suppresses_all_identity() {
        let dir = tempdir().unwrap();
        for name in ["net-root", "unknown-root"] {
            let root = dir.path().join(name);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("f.txt"), b"x").unwrap();
            std::fs::create_dir_all(root.join("sub")).unwrap();
            fs::write(root.join("sub/g.txt"), b"y").unwrap();

            let mut c = cfg(&root);
            c.want_identity = true;
            c.fs_mock = mock_fs();
            let out = run(c, Arc::new(Progress::new()), Arc::new(Control::new()));

            assert!(
                !out.tree.id(0).is_known(),
                "root ID must be UNKNOWN: {name}"
            );
            assert_unknown(&out, &["f.txt"]);
            assert_unknown(&out, &["sub"]);
            assert_unknown(&out, &["sub", "g.txt"]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn followed_link_uses_target_scope_not_parent_scope() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(outside.join("net2")).unwrap();
        fs::write(outside.join("net2/n.txt"), b"net").unwrap();
        fs::create_dir_all(outside.join("local2")).unwrap();
        fs::write(outside.join("local2/l.txt"), b"local").unwrap();
        symlink(outside.join("net2"), root.join("to_net")).unwrap();
        symlink(outside.join("local2"), root.join("to_local")).unwrap();
        symlink(outside.join("net2/n.txt"), root.join("to_net_file")).unwrap();

        let mut c = cfg(&root);
        c.want_identity = true;
        c.follow_links = true;
        c.threads = 1;
        c.fs_mock = mock_fs();
        let out = run(c, Arc::new(Progress::new()), Arc::new(Control::new()));

        // リンク自体はローカルにあるため ID を持つ。
        assert_known(&out, &["to_net"]);
        assert_known(&out, &["to_local"]);
        // リンク先の分類は独立しており、ネットワーク先の子孫は UNKNOWN。
        assert_unknown(&out, &["to_net", "n.txt"]);
        assert_known(&out, &["to_local", "l.txt"]);

        // ファイルリンクはリンク自身の ID を保持し、リンク先の ID は使わない。
        let link_idx = find_node(&out, &["to_net_file"]).unwrap();
        let link_path = root.join("to_net_file");
        let link_md = std::fs::symlink_metadata(&link_path).unwrap();
        let link_id = platform::file_id(&link_md, &link_path, true).unwrap();
        let target_md = std::fs::metadata(outside.join("net2/n.txt")).unwrap();
        let target_id = platform::file_id(&target_md, &outside.join("net2/n.txt"), false).unwrap();
        assert_eq!(out.tree.id(link_idx), link_id);
        assert_ne!(link_id, target_id);
    }

    #[cfg(unix)]
    #[test]
    fn local_only_uses_legacy_network_flag_not_identity_class() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("fuse")).unwrap();
        fs::write(root.join("fuse/f.txt"), b"fuse").unwrap();
        fs::create_dir_all(root.join("unknown")).unwrap();
        fs::write(root.join("unknown/u.txt"), b"unknown").unwrap();
        fs::create_dir_all(root.join("local")).unwrap();
        fs::write(root.join("local/l.txt"), b"local").unwrap();

        let mut c = cfg(&root);
        c.want_identity = true;
        c.local_only = true;
        c.threads = 1;
        c.fs_mock = mock_fs();
        let out = run(c, Arc::new(Progress::new()), Arc::new(Control::new()));

        let fuse = find_node(&out, &["fuse"]).unwrap();
        assert_eq!(out.tree.node(fuse).kind, NodeKind::Blocked);
        // FUSE は local_only_network=true のため走査対象外。
        assert_unknown(&out, &["fuse"]);
        assert!(find_node(&out, &["fuse", "f.txt"]).is_none());
        // Unknown は既存 --local-only では除外しないが、ID は収集しない。
        assert!(find_node(&out, &["unknown", "u.txt"]).is_some());
        assert_unknown(&out, &["unknown"]);
        assert_unknown(&out, &["unknown", "u.txt"]);
        assert_known(&out, &["local", "l.txt"]);
    }
}
