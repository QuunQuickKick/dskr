use std::fs;
use std::io::{stdout, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use dskr::app::App;
use dskr::cli::{
    Cli, ColorArg, LoadPreset, LoadValues, MetricOpt, OutputFormatArg, ProfileOpt, UnitArg,
    UnitSystemArg,
};
use dskr::format::{parse_size, SizeFormat, UnitDisplay, UnitSystem};
use dskr::output::{self, OutputFormat, OutputOptions};
use dskr::path::{classify_root, RootKind};
use dskr::report::{self, ReportMeta};
use dskr::scan::engine::{Control, ControlState, EngineConfig, Progress};
use dskr::scan::run;
use dskr::sort::{SortDir, SortField, SortSpec};
use dskr::ui;

enum ScanMsg {
    Done(Box<dskr::scan::ScanOutcome>),
    Failed(String),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match try_main(cli) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(130) => {
            eprintln!("dskr: 中止されました");
            ExitCode::from(130)
        }
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dskr: {e}");
            ExitCode::FAILURE
        }
    }
}

fn try_main(cli: Cli) -> Result<u32, String> {
    let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let kind = classify_root(&root)?;
    let md = fs::metadata(&root)
        .map_err(|e| format!("パスを参照できません: {} ({e})", root.display()))?;
    if !md.is_dir() {
        return Err(format!(
            "ディレクトリを指定してください: {}",
            root.display()
        ));
    }

    #[cfg(windows)]
    if cli.nice.is_some() {
        eprintln!("警告: --nice は Windows では使用できません");
    }
    #[cfg(not(windows))]
    if cli.skip_cloud_placeholders {
        eprintln!("警告: --skip-cloud-placeholders は Windows のみ対応です");
    }

    let network_profile = match cli.profile {
        ProfileOpt::Auto => kind.is_network(),
        ProfileOpt::Local => false,
        ProfileOpt::Network => true,
    };

    // --local-only: ネットワークFSへのアクセスを一切行わない
    if cli.local_only {
        if cli.follow_links {
            eprintln!("警告: --local-only 指定のためシンボリックリンクは辿りません");
        }
        if kind.is_network() {
            return Err(
                "--local-only 指定のためネットワークパス (UNC/ネットワークドライブ) は走査しません"
                    .into(),
            );
        }
        if dskr::scan::platform::is_network_fs(&root) {
            return Err(
                "--local-only 指定のためネットワークファイルシステム上のパスは走査しません".into(),
            );
        }
    }

    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let preset = LoadValues::resolve(cli.load, network_profile, cpus);
    let (mut threads, mut nice, mut io, mut rate) =
        (preset.threads, preset.nice, preset.io, preset.rate);
    if cli.one_file_system && cli.follow_links {
        eprintln!("警告: -x 指定のためシンボリックリンクは辿りません");
    }
    if let Some(t) = cli.threads {
        // スレッド爆発によるリソース枯渇を防ぐ
        if t > 256 {
            eprintln!("警告: --threads は最大 256 に制限します");
        }
        threads = t.clamp(1, 256);
    }
    if cli.nice.is_some() {
        nice = cli.nice;
    }
    if cli.io_priority.is_some() {
        io = cli.io_priority;
    }
    if let Some(r) = cli.max_entries_per_sec {
        rate = r.max(50);
    }

    for w in dskr::scan::platform::apply_priority(nice, io) {
        eprintln!("警告: {w}");
    }

    let want_allocated = match cli.metric {
        MetricOpt::Allocated => true,
        MetricOpt::Logical => false,
        MetricOpt::Auto => !network_profile,
    };
    let min_size = match &cli.min_size {
        Some(s) => parse_size(s).ok_or_else(|| format!("--min-size の形式が不正です: {s}"))?,
        None => 0,
    };
    // ノード番号は u32 のため、上限を u32::MAX にクランプする
    let max_nodes = if cli.max_nodes > u32::MAX as u64 {
        eprintln!("警告: --max-nodes は {} に制限します", u32::MAX);
        u32::MAX as u64
    } else {
        cli.max_nodes
    };
    let skip_pseudo = cfg!(target_os = "linux") && root.as_os_str() == std::ffi::OsStr::new("/");

    let cfg = EngineConfig {
        root: root.clone(),
        follow_links: cli.follow_links,
        one_file_system: cli.one_file_system,
        excludes: cli.exclude.clone(),
        include_hidden: cli.hidden,
        min_size,
        max_depth: cli.max_depth,
        max_nodes,
        threads,
        rate_limit: rate,
        skip_cloud_placeholders: cli.skip_cloud_placeholders,
        want_allocated,
        want_identity: cli.allow_delete,
        skip_pseudo,
        network: network_profile,
        local_only: cli.local_only,
    };

    let progress = Arc::new(Progress::new());
    let control = Arc::new(Control::new());
    if cli.pause_on_start {
        control.pause();
    }

    let profile_label = if network_profile { "network" } else { "local" }.to_string();
    let options_json = options_json(&cli, threads, rate, profile_label.as_str());
    let sort_spec: SortSpec = match &cli.sort {
        Some(s) => s.parse()?,
        None => SortSpec {
            field: SortField::Size,
            dir: SortDir::Desc,
        },
    };
    let size_format = to_size_format(cli.units, cli.unit);

    if matches!(cli.format, Some(OutputFormatArg::Json)) && cli.json.is_some() {
        return Err("--format json と --json は同時に指定できません".into());
    }
    if matches!(cli.format, Some(OutputFormatArg::Csv)) && cli.csv.is_some() {
        return Err("--format csv と --csv は同時に指定できません".into());
    }
    if cli.allow_delete && headless_requested(&cli) {
        return Err(
            "--allow-delete は TUI でのみ使用できます (--no-tui / --format などとは併用できません)"
                .into(),
        );
    }

    if headless_requested(&cli) {
        headless(
            &cli,
            cfg,
            progress,
            control,
            kind,
            &profile_label,
            threads,
            &options_json,
            sort_spec,
            size_format,
        )
    } else {
        tui(TuiParams {
            cfg,
            progress,
            control,
            root,
            kind,
            profile_label,
            threads,
            show_allocated: want_allocated,
            allow_delete: cli.allow_delete,
            options_json,
            sort_spec,
            size_format,
        })
    }
}

fn headless_requested(cli: &Cli) -> bool {
    cli.no_tui
        || cli.top.is_some()
        || cli.format.is_some()
        || cli.output.is_some()
        || cli.json.is_some()
        || cli.csv.is_some()
}

fn to_output_format(arg: OutputFormatArg) -> OutputFormat {
    match arg {
        OutputFormatArg::Report => OutputFormat::Report,
        OutputFormatArg::Tree => OutputFormat::Tree,
        OutputFormatArg::Flat => OutputFormat::Flat,
        OutputFormatArg::Markdown => OutputFormat::Markdown,
        OutputFormatArg::Html => OutputFormat::Html,
        OutputFormatArg::Json => OutputFormat::Json,
        OutputFormatArg::Csv => OutputFormat::Csv,
    }
}

fn to_size_format(units: UnitSystemArg, unit: UnitArg) -> SizeFormat {
    let system = match units {
        UnitSystemArg::Iec => UnitSystem::Iec,
        UnitSystemArg::Si => UnitSystem::Si,
    };
    let display = match unit {
        UnitArg::Auto => UnitDisplay::Auto,
        UnitArg::B => UnitDisplay::Fixed(0),
        UnitArg::Kib => UnitDisplay::Fixed(1),
        UnitArg::Mib => UnitDisplay::Fixed(2),
        UnitArg::Gib => UnitDisplay::Fixed(3),
        UnitArg::Tib => UnitDisplay::Fixed(4),
        UnitArg::Pib => UnitDisplay::Fixed(5),
        UnitArg::Eib => UnitDisplay::Fixed(6),
    };
    SizeFormat { system, display }
}

fn color_enabled(choice: ColorArg, to_file: bool) -> bool {
    match choice {
        ColorArg::Always => true,
        ColorArg::Never => false,
        ColorArg::Auto => {
            !to_file
                && std::io::stdout().is_terminal()
                && (!cfg!(windows) || std::env::var_os("WT_SESSION").is_some())
        }
    }
}

fn options_json(cli: &Cli, threads: usize, rate: u64, profile: &str) -> String {
    let load = match cli.load {
        LoadPreset::Low => "low",
        LoadPreset::Normal => "normal",
        LoadPreset::Max => "max",
    };
    let excl = cli
        .exclude
        .iter()
        .map(|e| report::json_string(e))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"load\":\"{load}\",\"profile\":\"{profile}\",\"follow_links\":{},\"one_file_system\":{},\"excludes\":[{excl}],\"hidden\":{},\"min_size\":{},\"max_depth\":{},\"max_nodes\":{},\"rate_limit\":{},\"skip_cloud_placeholders\":{},\"threads\":{}}}",
        cli.follow_links,
        cli.one_file_system,
        cli.hidden,
        cli.min_size.as_deref().and_then(parse_size).unwrap_or(0),
        cli.max_depth.map(|d| d.to_string()).unwrap_or_else(|| "null".into()),
        cli.max_nodes,
        rate,
        cli.skip_cloud_placeholders,
        threads,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_output_target(
    path: Option<&Path>,
    outcome: &dskr::scan::ScanOutcome,
    meta: &ReportMeta,
    opts: &OutputOptions,
) -> Result<(), String> {
    if let Some(p) = path.filter(|p| *p != Path::new("-")) {
        let file = report::create_private(p, false)
            .map_err(|e| format!("出力先を開けません {}: {e}", p.display()))?;
        let mut w = std::io::BufWriter::new(file);
        output::write_output(&mut w, outcome, meta, opts)
            .map_err(|e| format!("書き出しに失敗 {}: {e}", p.display()))?;
        use std::io::Write;
        w.flush()
            .map_err(|e| format!("書き出しに失敗 {}: {e}", p.display()))?;
        eprintln!("出力: {}", p.display());
        Ok(())
    } else {
        let stdout = std::io::stdout();
        let mut w = std::io::BufWriter::new(stdout.lock());
        match output::write_output(&mut w, outcome, meta, opts) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            Err(e) => return Err(format!("出力に失敗: {e}")),
        }
        use std::io::Write;
        match w.flush() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(e) => Err(format!("出力に失敗: {e}")),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn headless(
    cli: &Cli,
    cfg: EngineConfig,
    progress: Arc<Progress>,
    control: Arc<Control>,
    kind: RootKind,
    profile_label: &str,
    threads: usize,
    options_json: &str,
    sort_spec: SortSpec,
    size_format: SizeFormat,
) -> Result<u32, String> {
    let cfg_root = cfg.root.clone();
    let want_allocated = cfg.want_allocated;
    let allocated_collected = !cfg!(windows) || want_allocated || !cfg.network;
    let outcome = run(cfg, progress, control);
    let metric = if want_allocated {
        "allocated"
    } else {
        "logical"
    };
    let meta = report::build_meta(
        &outcome,
        &cfg_root.to_string_lossy(),
        kind.label(),
        profile_label,
        threads,
        options_json,
        metric,
        allocated_collected,
    );

    let explicit_format = cli.format.map(to_output_format);
    let primary = match explicit_format {
        Some(f) => Some(f),
        None => {
            if cli.no_tui || cli.top.is_some() || cli.output.is_some() {
                Some(OutputFormat::Report)
            } else {
                None
            }
        }
    };

    if let Some(format) = primary {
        let depth = cli.depth.unwrap_or_else(|| format.default_depth());
        let top = cli.top.unwrap_or_else(|| format.default_top());
        let path = cli.output.as_deref();
        let to_file = matches!(path, Some(p) if p != Path::new("-"));
        let opts = OutputOptions {
            format,
            depth,
            top,
            sort_field: sort_spec.field,
            sort_dir: sort_spec.dir,
            size: size_format,
            ascii: cli.ascii,
            color: color_enabled(cli.color, to_file),
            metric_alloc: want_allocated,
        };
        write_output_target(path, &outcome, &meta, &opts)?;
    }

    // 既存の --json / --csv 指定は追加出力として維持する
    if let Some(j) = &cli.json {
        let opts = OutputOptions {
            format: OutputFormat::Json,
            depth: 0,
            top: 0,
            sort_field: sort_spec.field,
            sort_dir: sort_spec.dir,
            size: size_format,
            ascii: false,
            color: false,
            metric_alloc: want_allocated,
        };
        write_output_target(Some(j), &outcome, &meta, &opts)?;
    }
    if let Some(c) = &cli.csv {
        let opts = OutputOptions {
            format: OutputFormat::Csv,
            depth: 0,
            top: 0,
            sort_field: sort_spec.field,
            sort_dir: sort_spec.dir,
            size: size_format,
            ascii: false,
            color: false,
            metric_alloc: want_allocated,
        };
        write_output_target(Some(c), &outcome, &meta, &opts)?;
    }

    Ok(if outcome.cancelled { 130 } else { 0 })
}

/// TUI 起動に必要な一式。try_main で組み立てて tui() へ渡す。
struct TuiParams {
    cfg: EngineConfig,
    progress: Arc<Progress>,
    control: Arc<Control>,
    root: PathBuf,
    kind: RootKind,
    profile_label: String,
    threads: usize,
    show_allocated: bool,
    allow_delete: bool,
    options_json: String,
    sort_spec: SortSpec,
    size_format: SizeFormat,
}

fn tui(p: TuiParams) -> Result<u32, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    if let Err(e) = execute!(stdout(), EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.to_string());
    }

    // パニック時も端末を復元する
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    let res = run_tui(p);

    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen);

    match res {
        Ok((code, force)) => {
            if force {
                // 応答しないネットワーク等で走査スレッドが残っていても即終了。
                // 端末は復元済み。
                std::process::exit(130);
            }
            Ok(code)
        }
        Err(e) => Err(e),
    }
}

fn run_tui(p: TuiParams) -> Result<(u32, bool), String> {
    let TuiParams {
        cfg,
        progress,
        control,
        root,
        kind,
        profile_label,
        threads,
        show_allocated,
        allow_delete,
        options_json,
        sort_spec,
        size_format,
    } = p;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout())).map_err(|e| e.to_string())?;
    let _ = terminal.clear();

    // 走査スレッド
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let cfg = cfg.clone();
        let progress = progress.clone();
        let control = control.clone();
        std::thread::Builder::new()
            .name("dskr-scan".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(cfg, progress, control)
                }));
                let msg = match result {
                    Ok(out) => ScanMsg::Done(Box::new(out)),
                    Err(_) => {
                        ScanMsg::Failed("内部エラー: 走査スレッドが異常終了しました".to_string())
                    }
                };
                let _ = tx.send(msg);
            })
            .map_err(|e| e.to_string())?;
    }

    let mut app = App::new(
        root,
        kind,
        profile_label,
        show_allocated,
        allow_delete,
        progress,
        control,
        threads,
        options_json,
    );
    app.sort_field = sort_spec.field;
    app.sort_dir = sort_spec.dir;
    app.size_format = size_format;

    loop {
        app.paused = app.control.state() == ControlState::Paused;
        terminal
            .draw(|f| ui::draw(f, &mut app))
            .map_err(|e| e.to_string())?;

        if event::poll(Duration::from_millis(100)).map_err(|e| e.to_string())? {
            if let Event::Key(k) = event::read().map_err(|e| e.to_string())? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k);
                }
            }
        }
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ScanMsg::Done(out) => app.finish(*out),
                ScanMsg::Failed(m) => app.fail(m),
            }
        }
        if app.force_quit {
            break;
        }
        if app.quit && !app.scanning {
            break;
        }
    }

    let _ = terminal.show_cursor();
    Ok((if app.cancel_requested { 130 } else { 0 }, app.force_quit))
}
