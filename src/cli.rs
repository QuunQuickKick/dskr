use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LoadPreset {
    /// 裏で回しても負荷を抑える (スレッドを CPU/4・最大8、優先度を下げ、ネットワークのみレート制限)
    Low,
    /// 通常 (ローカルは論理CPU数、ネットワークは8〜32スレッド)
    Normal,
    /// 最大 (スレッドを通常の2倍。SSD/NVMe・ネットワーク向け)
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ProfileOpt {
    /// 自動判定 (Windows: UNC/ネットワークドライブ。Unix: 通常ローカル)
    Auto,
    /// ローカルとして扱う
    Local,
    /// ネットワーク向け設定を強制 (Unix の NFS/SMB など)
    Network,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum IoPriority {
    /// 最も低い I/O 優先度
    Idle,
    /// 低い I/O 優先度
    Low,
    /// 通常 (OS の既定)
    Normal,
}

/// `--load` と環境 (ローカル/ネットワーク、論理CPU数) から解決した実効値。
/// 個別オプション (`--threads` など) による上書きは呼び出し側で行う。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadValues {
    pub threads: usize,
    pub nice: Option<i32>,
    pub io: Option<IoPriority>,
    /// 1秒あたりの項目数上限 (全ワーカー合計)。0 は無制限。
    pub rate: u64,
}

impl LoadValues {
    /// `cpus` は論理 CPU 数、`network` はネットワークプロファイルかどうか。
    pub fn resolve(load: LoadPreset, network: bool, cpus: usize) -> Self {
        let cpus = cpus.max(1);
        // ネットワークはレイテンシ隠蔽のためスレッドを多めにする。
        // low / max はこの基準を分割・倍加する。
        let base = if network {
            cpus.saturating_mul(4).clamp(8, 32)
        } else {
            cpus
        };
        match load {
            LoadPreset::Low => LoadValues {
                threads: base.div_ceil(4).clamp(1, 8),
                nice: Some(10),
                io: Some(IoPriority::Idle),
                // ローカルはスレッド数と優先度に任せ、ネットワークのみレートを制限する
                rate: if network { 2_000 } else { 0 },
            },
            LoadPreset::Normal => LoadValues {
                threads: base,
                nice: None,
                io: None,
                rate: 0,
            },
            LoadPreset::Max => LoadValues {
                threads: base.saturating_mul(2).clamp(2, 64),
                nice: None,
                io: None,
                rate: 0,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MetricOpt {
    /// 自動: ローカルはディスク上の使用量、ネットワークは見かけのサイズ
    Auto,
    /// ディスク上の使用量
    Allocated,
    /// 見かけのサイズ
    Logical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormatArg {
    /// 概要レポート (画面を使わないときの既定)
    Report,
    /// フォルダ構成
    Tree,
    /// du 風のフラット一覧
    Flat,
    /// Markdown レポート
    Markdown,
    /// 単一 HTML レポート
    Html,
    Json,
    Csv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum UnitSystemArg {
    /// 1024 基準 (KiB/MiB/GiB)
    Iec,
    /// 1000 基準 (KB/MB/GB)
    Si,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum UnitArg {
    Auto,
    B,
    Kib,
    Mib,
    Gib,
    Tib,
    Pib,
    Eib,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
    Auto,
    Always,
    Never,
}

#[derive(Parser, Debug)]
#[cfg_attr(
    windows,
    command(after_help = r#"例:
  dskr C:\Users                              # TUI で探索
  dskr \\server\share --profile network      # UNC パス
  dskr \\?\UNC\server\share\data             # 拡張長パス
  dskr C:\Users --allow-delete               # TUI で削除も可能にする
  dskr C:\Users --no-tui                     # サマリーレポート
  dskr C:\Users --format html --output report.html
"#)
)]
#[cfg_attr(
    target_os = "macos",
    command(after_help = r#"例:
  dskr ~/Library                             # TUI で探索
  dskr /Volumes/nas --profile network --load low
  dskr ~/Library --format tree --depth 2
  dskr ~/Library --format html --output report.html
  dskr /var --no-tui --json out.json --csv out.csv
  dskr ~/Library --allow-delete
"#)
)]
#[cfg_attr(
    all(unix, not(target_os = "macos")),
    command(after_help = r#"例:
  dskr /var                                  # TUI で探索
  dskr /mnt/nas --profile network --load low
  dskr /var --format tree --depth 2
  dskr /var --format html --output report.html
  dskr /var --no-tui --json out.json --csv out.csv
  dskr /var --allow-delete
"#)
)]
#[command(
    name = "dskr",
    version,
    about = "指定ディレクトリ配下のディスク使用量を高速に調べ、表示するツール"
)]
pub struct Cli {
    /// 走査対象ディレクトリ (省略時はカレント)
    pub path: Option<PathBuf>,

    /// 負荷プリセット
    #[arg(short = 'l', long, value_enum, default_value_t = LoadPreset::Normal)]
    pub load: LoadPreset,

    #[arg(short = 'p', long, value_enum, default_value_t = ProfileOpt::Auto)]
    #[cfg_attr(
        windows,
        arg(help = "プロファイル (auto は UNC / ネットワークドライブを自動判定)")
    )]
    #[cfg_attr(
        target_os = "macos",
        arg(help = "プロファイル (auto は通常ローカル。ネットワークは network を指定)")
    )]
    #[cfg_attr(
        all(unix, not(target_os = "macos")),
        arg(help = "プロファイル (auto は通常ローカル。NFS/SMB などは network を指定)")
    )]
    pub profile: ProfileOpt,

    /// 走査ワーカースレッド数 (既定: プリセットに従う)
    #[arg(short = 'j', long)]
    pub threads: Option<usize>,

    #[arg(long)]
    #[cfg_attr(windows, arg(hide = true))]
    #[cfg_attr(unix, arg(help = "nice 値を適用 (0-19)"))]
    pub nice: Option<i32>,

    #[arg(long, value_enum)]
    #[cfg_attr(
        windows,
        arg(help = "I/O 優先度 (プロセス優先度を下げ、EcoQoS を有効化)")
    )]
    #[cfg_attr(
        target_os = "macos",
        arg(help = "I/O 優先度 (macOS は未対応。nice のみ適用)")
    )]
    #[cfg_attr(
        target_os = "linux",
        arg(help = "I/O 優先度 (Linux: ioprio。未対応アーキテクチャでは警告)")
    )]
    #[cfg_attr(
        all(unix, not(any(target_os = "linux", target_os = "macos"))),
        arg(help = "I/O 優先度 (このプラットフォームでは未対応)")
    )]
    pub io_priority: Option<IoPriority>,

    /// 1秒あたりに調べる項目数の上限 (全ワーカー合計、最小50)
    #[arg(long)]
    pub max_entries_per_sec: Option<u64>,

    /// 保持ノード数の上限 (超過分は省略され truncated 表示)
    #[arg(long, default_value_t = crate::scan::MAX_NODES_DEFAULT)]
    pub max_nodes: u64,

    /// このサイズ未満のファイルを集計しない (例: 10M)
    #[arg(short = 'm', long)]
    pub min_size: Option<String>,

    /// この深さより深い階層を作らない (ルート = 0)
    #[arg(short = 'D', long)]
    pub max_depth: Option<u16>,

    #[arg(short = 'L', long)]
    #[cfg_attr(
        windows,
        arg(
            help = "シンボリックリンク / ジャンクションを辿る (ループ検出あり。同一ボリューム内のみ。-x / local-only では辿らない)"
        )
    )]
    #[cfg_attr(
        target_os = "macos",
        arg(help = "シンボリックリンクを辿る (ループ検出あり。-x / local-only では辿らない)")
    )]
    #[cfg_attr(
        all(unix, not(target_os = "macos")),
        arg(
            help = "シンボリックリンクを辿る (ループ検出あり。別FSへは -x / local-only 指定時に辿らない)"
        )
    )]
    pub follow_links: bool,

    #[arg(long)]
    #[cfg_attr(
        windows,
        arg(help = "UNC / ネットワークドライブ / 別ボリュームを走査せず、リンクも辿らない")
    )]
    #[cfg_attr(
        target_os = "macos",
        arg(help = "ネットワークマウント (NFS/SMB/WebDAV など) を走査せず、リンクも辿らない")
    )]
    #[cfg_attr(
        target_os = "linux",
        arg(help = "ネットワークFS (NFS/SMB/FUSE/9p など) を走査せず、リンクも辿らない")
    )]
    #[cfg_attr(
        all(unix, not(any(target_os = "linux", target_os = "macos"))),
        arg(help = "ネットワークFSの自動判定には未対応 (リンクは辿らない)")
    )]
    pub local_only: bool,

    /// ファイルシステム境界を越えない (-x)
    #[arg(short = 'x', long)]
    pub one_file_system: bool,

    /// 除外パターン (ファイル名に対する glob, * と ? のみ)
    #[arg(short = 'e', long)]
    pub exclude: Vec<String>,

    /// 隠しファイル・システムファイルも表示する (既定: 非表示)
    #[arg(short = 'H', long)]
    pub hidden: bool,

    #[arg(long)]
    #[cfg_attr(
        windows,
        arg(help = "クラウド プレースホルダ (OneDrive 等) を集計対象から除外")
    )]
    #[cfg_attr(not(windows), arg(hide = true))]
    pub skip_cloud_placeholders: bool,

    /// 合計に使うサイズの種類 (auto は自動)
    #[arg(long, value_enum, default_value_t = MetricOpt::Auto)]
    pub metric: MetricOpt,

    /// 出力形式 (指定時はヘッドレス)
    #[arg(short = 'f', long, value_enum)]
    pub format: Option<OutputFormatArg>,

    /// 出力先 (省略時は標準出力。`-` も標準出力)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// ツリー/Markdown/HTML の深さ (ルート = 0)
    #[arg(short = 'd', long)]
    pub depth: Option<usize>,

    /// ソート指定 (size|name|count|mtime[:asc|desc])
    #[arg(short = 's', long)]
    pub sort: Option<String>,

    /// 単位の基準 (iec: 1024基準 / si: 1000基準)
    #[arg(short = 'u', long, value_enum, default_value_t = UnitSystemArg::Iec)]
    pub units: UnitSystemArg,

    /// 表示単位 (auto は自動)
    #[arg(short = 'U', long, value_enum, default_value_t = UnitArg::Auto)]
    pub unit: UnitArg,

    /// ツリーの罫線を ASCII にする
    #[arg(short = 'a', long)]
    pub ascii: bool,

    #[arg(short = 'C', long, value_enum, default_value_t = ColorArg::Auto)]
    #[cfg_attr(
        windows,
        arg(help = "色付け (auto は Windows Terminal など色対応端末のときのみ)")
    )]
    #[cfg_attr(not(windows), arg(help = "色付け (auto は端末出力時のみ)"))]
    pub color: ColorArg,

    /// JSON レポートの出力先 (--format json の別名)
    #[arg(short = 'J', long)]
    pub json: Option<PathBuf>,

    /// CSV レポートの出力先 (--format csv の別名)
    #[arg(short = 'c', long)]
    pub csv: Option<PathBuf>,

    /// 上位 N 件 (0 で無制限)。report/tree/flat の件数に使用
    #[arg(short = 't', long)]
    pub top: Option<usize>,

    /// TUI を使わず結果を標準出力する (既定: report)
    #[arg(short = 'n', long)]
    pub no_tui: bool,

    /// 走査を一時停止状態で開始 (TUI で Space により再開)
    #[arg(long)]
    pub pause_on_start: bool,

    /// TUI で選択項目の削除を許可する (確認付きの完全削除。既定は無効)
    #[arg(long)]
    pub allow_delete: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn help() -> String {
        Cli::command().render_long_help().to_string()
    }

    #[test]
    fn help_matches_platform() {
        let help = help();
        #[cfg(windows)]
        {
            assert!(help.contains(r"\\server\share"), "{help}");
            assert!(help.contains("EcoQoS"), "{help}");
            assert!(help.contains("OneDrive"), "{help}");
            assert!(!help.contains("--nice"), "{help}");
            assert!(!help.contains("ioprio"), "{help}");
            assert!(!help.contains("macOS は未対応"), "{help}");
        }
        #[cfg(target_os = "macos")]
        {
            assert!(help.contains("~/Library"), "{help}");
            assert!(help.contains("/Volumes"), "{help}");
            assert!(help.contains("macOS は未対応"), "{help}");
            assert!(help.contains("--nice"), "{help}");
            assert!(!help.contains("--skip-cloud-placeholders"), "{help}");
            assert!(!help.contains("OneDrive"), "{help}");
            assert!(!help.contains("EcoQoS"), "{help}");
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert!(help.contains("/mnt/nas"), "{help}");
            assert!(help.contains("ioprio"), "{help}");
            assert!(help.contains("--nice"), "{help}");
            assert!(!help.contains("--skip-cloud-placeholders"), "{help}");
            assert!(!help.contains("OneDrive"), "{help}");
            assert!(!help.contains("EcoQoS"), "{help}");
        }
    }

    #[test]
    fn load_preset_threads() {
        // (cpus, network, preset) -> threads
        let cases = [
            (2, false, LoadPreset::Low, 1),
            (2, false, LoadPreset::Normal, 2),
            (2, false, LoadPreset::Max, 4),
            (4, false, LoadPreset::Low, 1),
            (4, false, LoadPreset::Normal, 4),
            (4, false, LoadPreset::Max, 8),
            (8, false, LoadPreset::Low, 2),
            (8, false, LoadPreset::Normal, 8),
            (8, false, LoadPreset::Max, 16),
            (16, false, LoadPreset::Low, 4),
            (16, false, LoadPreset::Normal, 16),
            (16, false, LoadPreset::Max, 32),
            (64, false, LoadPreset::Low, 8),
            (64, false, LoadPreset::Normal, 64),
            (64, false, LoadPreset::Max, 64),
            (2, true, LoadPreset::Low, 2),
            (2, true, LoadPreset::Normal, 8),
            (2, true, LoadPreset::Max, 16),
            (4, true, LoadPreset::Low, 4),
            (4, true, LoadPreset::Normal, 16),
            (4, true, LoadPreset::Max, 32),
            (8, true, LoadPreset::Low, 8),
            (8, true, LoadPreset::Normal, 32),
            (8, true, LoadPreset::Max, 64),
            (16, true, LoadPreset::Low, 8),
            (16, true, LoadPreset::Normal, 32),
            (16, true, LoadPreset::Max, 64),
            (64, true, LoadPreset::Low, 8),
            (64, true, LoadPreset::Normal, 32),
            (64, true, LoadPreset::Max, 64),
        ];
        for (cpus, network, load, threads) in cases {
            let got = LoadValues::resolve(load, network, cpus).threads;
            assert_eq!(
                got, threads,
                "cpus={cpus}, network={network}, load={load:?}"
            );
        }
    }

    #[test]
    fn load_preset_rate_and_priority() {
        let local_low = LoadValues::resolve(LoadPreset::Low, false, 8);
        assert_eq!(local_low.rate, 0);
        assert_eq!(local_low.nice, Some(10));
        assert_eq!(local_low.io, Some(IoPriority::Idle));

        let net_low = LoadValues::resolve(LoadPreset::Low, true, 8);
        assert_eq!(net_low.rate, 2_000);

        for load in [LoadPreset::Normal, LoadPreset::Max] {
            let v = LoadValues::resolve(load, false, 8);
            assert_eq!(v.rate, 0);
            assert_eq!(v.nice, None);
            assert_eq!(v.io, None);
        }
    }

    #[test]
    fn short_flags_parse() {
        let cli = Cli::try_parse_from([
            "dskr", ".", "-l", "low", "-p", "network", "-j", "4", "-m", "10M", "-D", "3", "-L",
            "-x", "-e", "*.tmp", "-H", "-f", "tree", "-o", "out.txt", "-d", "2", "-s", "name:asc",
            "-u", "si", "-U", "mib", "-a", "-C", "never", "-J", "out.json", "-c", "out.csv", "-t",
            "50", "-n",
        ])
        .unwrap();

        assert_eq!(cli.load, LoadPreset::Low);
        assert_eq!(cli.profile, ProfileOpt::Network);
        assert_eq!(cli.threads, Some(4));
        assert_eq!(cli.min_size.as_deref(), Some("10M"));
        assert_eq!(cli.max_depth, Some(3));
        assert!(cli.follow_links);
        assert!(cli.one_file_system);
        assert_eq!(cli.exclude, vec!["*.tmp"]);
        assert!(cli.hidden);
        assert_eq!(cli.format, Some(OutputFormatArg::Tree));
        assert_eq!(cli.output.as_deref(), Some(std::path::Path::new("out.txt")));
        assert_eq!(cli.depth, Some(2));
        assert_eq!(cli.sort.as_deref(), Some("name:asc"));
        assert_eq!(cli.units, UnitSystemArg::Si);
        assert_eq!(cli.unit, UnitArg::Mib);
        assert!(cli.ascii);
        assert_eq!(cli.color, ColorArg::Never);
        assert_eq!(cli.json.as_deref(), Some(std::path::Path::new("out.json")));
        assert_eq!(cli.csv.as_deref(), Some(std::path::Path::new("out.csv")));
        assert_eq!(cli.top, Some(50));
        assert!(cli.no_tui);
    }

    #[cfg(windows)]
    #[test]
    fn hidden_nice_still_parses() {
        let cli = Cli::try_parse_from(["dskr", ".", "--nice", "5"]).unwrap();
        assert_eq!(cli.nice, Some(5));
    }

    #[cfg(not(windows))]
    #[test]
    fn hidden_cloud_placeholder_still_parses() {
        let cli = Cli::try_parse_from(["dskr", ".", "--skip-cloud-placeholders"]).unwrap();
        assert!(cli.skip_cloud_placeholders);
    }
}
