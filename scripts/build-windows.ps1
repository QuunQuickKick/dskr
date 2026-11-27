#requires -Version 5.1
<#
.SYNOPSIS
    dskr の Windows 向けリリースビルドスクリプト。

.DESCRIPTION
    Rust (MSVC ツールチェーン) でビルドし、dist に exe / zip と SHA-256 を出力します。
    初回は crates.io からの依存取得のためインターネット接続が必要です。
    オフライン環境では、事前に vendor ディレクトリを用意して -Offline を指定してください。

.PARAMETER Configuration
    Release (既定) または Debug。

.PARAMETER Test
    ビルド前に cargo test を実行します。

.PARAMETER Zip
    README / USAGE / ACCURACY / ライセンス / SHA256SUMS.txt をバージョン付きフォルダに
    同梱した配布用 zip を作成します。zip 自体の SHA-256 も dist に出力します。

.PARAMETER Clean
    ビルド前に cargo clean を実行します。

.PARAMETER NoSmoke
    生成した exe の起動確認 (スモークテスト) を省略します。

.PARAMETER Offline
    vendor ディレクトリを使ってオフラインビルドします。

.PARAMETER OutDir
    出力先ディレクトリ (既定: dist)。

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Test -Zip

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Clean -Offline
#>
[CmdletBinding()]
param(
    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Release',

    [switch]$Test,

    [switch]$Zip,

    [switch]$Clean,

    [switch]$NoSmoke,

    [switch]$Offline,

    [string]$OutDir = 'dist'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# PowerShell 7.3+ ではネイティブコマンドの stderr を自動でエラー扱いにしない。
# Windows PowerShell 5.1 では未定義の変数への代入は無害。
$PSNativeCommandUseErrorActionPreference = $false

$script:Started = Get-Date
$script:LastCargoOutput = ''
$script:CapturedOutput = ''

function Write-Step {
    param([string]$Message)
    Write-Host ''
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Info {
    param([string]$Message)
    Write-Host "    $Message"
}

function Write-Warn {
    param([string]$Message)
    Write-Host "警告: $Message" -ForegroundColor Yellow
}

function Fail {
    param([string]$Message)
    Write-Host ''
    Write-Host "エラー: $Message" -ForegroundColor Red
    exit 1
}

<#
    ネイティブコマンドを実行し、標準出力/標準エラーを 1 行ずつ表示する。

    Windows PowerShell 5.1 では $ErrorActionPreference = 'Stop' のまま
    `2>&1` すると、ネイティブコマンドの stderr (cargo の "Compiling ..." など)
    が NativeCommandError として例外化される。実行中だけ 'Continue' に
    切り替えることでこれを避ける。終了コードを返す。
#>
function Invoke-Native {
    param(
        [string]$FilePath,
        [string[]]$Arguments,
        [switch]$Quiet
    )
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $captured = @()
    try {
        & $FilePath @Arguments 2>&1 | Tee-Object -Variable captured | ForEach-Object {
            $text = [string]$_
            # PowerShell が空のエラー行を型名で表示する場合があるため除去
            if ($text -eq 'System.Management.Automation.RemoteException') {
                $text = ''
            }
            if (-not $Quiet) {
                Write-Host $text
            }
        }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousPreference
    }
    $lines = @()
    foreach ($item in $captured) {
        $text = [string]$item
        if ($text -eq 'System.Management.Automation.RemoteException') {
            $text = ''
        }
        $lines += $text
    }
    $script:CapturedOutput = ($lines -join [Environment]::NewLine)
    return $code
}

function Invoke-Cargo {
    param([string[]]$Arguments)
    Write-Info ("cargo " + ($Arguments -join ' '))
    $code = Invoke-Native -FilePath 'cargo' -Arguments $Arguments
    $script:LastCargoOutput = $script:CapturedOutput
    if ($code -ne 0) {
        throw ("cargo が失敗しました (exit " + $code + ")。上記のメッセージを確認してください。")
    }
}

try {
    # ------------------------------------------------------------------
    # 0. 場所と環境の確認
    # ------------------------------------------------------------------
    $RepoRoot = Split-Path -Parent $PSScriptRoot
    if (-not (Test-Path (Join-Path $RepoRoot 'Cargo.toml'))) {
        Fail ("Cargo.toml が見つかりません: " + $RepoRoot)
    }
    Set-Location $RepoRoot

    Write-Step '環境を確認しています'

    if (-not [Environment]::Is64BitOperatingSystem) {
        Fail '64bit Windows が必要です。'
    }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail @'
Rust (cargo) が見つかりません。次のいずれかで導入してください。
  1) https://rustup.rs から rustup-init.exe を実行 (既定の MSVC ツールチェーン)
  2) winget install Rustlang.Rustup
導入後、新しいターミナルを開いて再実行してください。
'@
    }

    $hostTriple = ''
    $rustcExit = Invoke-Native -FilePath 'rustc' -Arguments @('-vV') -Quiet
    if ($rustcExit -ne 0) {
        throw 'rustc -vV の実行に失敗しました。Rust のインストールを確認してください。'
    }
    $rustcInfo = $script:CapturedOutput -split "\r?\n"
    if ($rustcInfo.Count -gt 0 -and $rustcInfo[0]) {
        Write-Info ("rustc: " + $rustcInfo[0])
    }
    foreach ($line in $rustcInfo) {
        if ($line -match '^host:\s*(.+)$') {
            $hostTriple = $Matches[1].Trim()
            break
        }
    }
    if (-not $hostTriple) {
        throw 'rustc のホスト情報を取得できませんでした。'
    }
    Write-Info ("rustc host: " + $hostTriple)

    $cargoExit = Invoke-Native -FilePath 'cargo' -Arguments @('--version') -Quiet
    if ($cargoExit -eq 0 -and $script:CapturedOutput.Trim()) {
        Write-Info ("cargo: " + $script:CapturedOutput.Trim())
    }

    if ($hostTriple -ne 'x86_64-pc-windows-msvc') {
        Write-Warn ("ホストが " + $hostTriple + " です。MSVC ツールチェーンを推奨します。")
        Write-Warn 'rustup toolchain install stable-x86_64-pc-windows-msvc'
        Write-Warn 'rustup default stable-x86_64-pc-windows-msvc'
    }

    $programFilesX86 = ${env:ProgramFiles(x86)}
    if (-not $programFilesX86) {
        $programFilesX86 = ${env:ProgramFiles}
    }
    if (-not $programFilesX86) {
        Write-Warn 'Program Files の場所を特定できませんでした。Visual Studio Build Tools を確認してください。'
    } else {
        $vswhere = Join-Path $programFilesX86 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path $vswhere) {
            $vswhereArgs = @(
                '-latest', '-products', '*',
                '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
                '-property', 'installationPath'
            )
            $vsExit = Invoke-Native -FilePath $vswhere -Arguments $vswhereArgs -Quiet
            $vsPath = $script:CapturedOutput.Trim()
            if ($vsExit -ne 0 -or -not $vsPath) {
                Write-Warn 'Visual Studio の C++ ビルドツールが見つかりませんでした。'
                Write-Warn 'リンク時に失敗する場合は Build Tools の「C++ によるデスクトップ開発」を導入してください。'
            } else {
                Write-Info ("Visual Studio: " + $vsPath)
            }
        } else {
            Write-Warn 'vswhere.exe が見つかりません。Visual Studio Build Tools (C++) が必要です。'
        }
    }

    if ($Offline) {
        if (-not (Test-Path (Join-Path $RepoRoot 'vendor'))) {
            Fail @'
オフラインビルドには vendor ディレクトリが必要です。
ネットワークのある環境で次を実行してからフォルダごとコピーしてください:
  cargo vendor vendor
  # .cargo\config.toml に vendor を参照する設定を追加
'@
        }
        Write-Info 'オフラインモード (vendor を使用)'
    }

    # ------------------------------------------------------------------
    # 1. クリーン
    # ------------------------------------------------------------------
    if ($Clean) {
        Write-Step 'ビルド成果物を削除しています'
        Invoke-Cargo @('clean')
    }

    # cargo 共通引数
    $cargoCommon = @()
    if ($Offline) {
        $cargoCommon += '--offline'
    }

    if (-not (Test-Path (Join-Path $RepoRoot 'Cargo.lock'))) {
        throw 'Cargo.lock が見つかりません。リポジトリをそのままコピーしたか確認してください。'
    }

    # ------------------------------------------------------------------
    # 2. テスト (任意)
    # ------------------------------------------------------------------
    if ($Test) {
        Write-Step 'テストを実行しています'
        $testArgs = @('test')
        if ($Configuration -eq 'Release') {
            $testArgs += '--release'
        }
        $testArgs += $cargoCommon
        Invoke-Cargo $testArgs
    }

    # ------------------------------------------------------------------
    # 3. ビルド
    # ------------------------------------------------------------------
    # 配布バイナリにビルド環境のローカルパスを残さない。
    $remapFlags = @("--remap-path-prefix=$RepoRoot=/src")
    if ($env:USERPROFILE) {
        $remapFlags += "--remap-path-prefix=$env:USERPROFILE=/build"
    }
    $remapText = $remapFlags -join ' '
    if ($env:RUSTFLAGS) {
        $env:RUSTFLAGS = $env:RUSTFLAGS + ' ' + $remapText
    } else {
        $env:RUSTFLAGS = $remapText
    }
    Write-Info ("RUSTFLAGS: " + $env:RUSTFLAGS)

    Write-Step ("ビルドしています (" + $Configuration + ")")
    $buildArgs = @('build', '--locked')
    if ($Configuration -eq 'Release') {
        $buildArgs += '--release'
    }
    $buildArgs += $cargoCommon
    Invoke-Cargo $buildArgs

    $profileDir = 'debug'
    if ($Configuration -eq 'Release') {
        $profileDir = 'release'
    }
    $builtExe = Join-Path $RepoRoot (Join-Path 'target' (Join-Path $profileDir 'dskr.exe'))
    if (-not (Test-Path $builtExe)) {
        throw ("生成物が見つかりません: " + $builtExe)
    }

    # ------------------------------------------------------------------
    # 4. dist へコピー
    # ------------------------------------------------------------------
    $version = ''
    foreach ($line in Get-Content -Path (Join-Path $RepoRoot 'Cargo.toml') -Encoding UTF8) {
        if ($line -match '^version\s*=\s*"([^"]+)"') {
            $version = $Matches[1]
            break
        }
    }
    if (-not $version) {
        $version = '0.0.0'
    }

    $outPath = $OutDir
    if (-not [System.IO.Path]::IsPathRooted($outPath)) {
        $outPath = Join-Path $RepoRoot $outPath
    }
    if (-not (Test-Path $outPath)) {
        New-Item -ItemType Directory -Path $outPath -Force | Out-Null
    }

    $suffix = 'windows-x86_64'
    $plainName = 'dskr.exe'
    if ($Configuration -ne 'Release') {
        $suffix += '-debug'
        $plainName = 'dskr-debug.exe'
    }

    $plainExe = Join-Path $outPath $plainName
    Copy-Item -Path $builtExe -Destination $plainExe -Force
    $versionedExe = Join-Path $outPath ("dskr-" + $version + "-" + $suffix + ".exe")
    Copy-Item -Path $builtExe -Destination $versionedExe -Force

    # ------------------------------------------------------------------
    # 5. 起動確認
    # ------------------------------------------------------------------
    if (-not $NoSmoke) {
        Write-Step '起動確認 (スモークテスト)'
        $previousEncoding = [Console]::OutputEncoding
        try {
            [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
            $smokeExit = Invoke-Native -FilePath $plainExe -Arguments @(
                $RepoRoot, '--no-tui', '--max-depth', '1', '--top', '5'
            )
        } finally {
            [Console]::OutputEncoding = $previousEncoding
        }
        if ($smokeExit -ne 0) {
            throw ("スモークテストに失敗しました (exit " + $smokeExit + ")。")
        }
        Write-Info '起動確認 OK'
    }

    # ------------------------------------------------------------------
    # 6. ハッシュと zip
    # ------------------------------------------------------------------
    $hash = (Get-FileHash -Path $versionedExe -Algorithm SHA256).Hash

    if ($Zip) {
        Write-Step 'zip を作成しています'
        $packageName = "dskr-" + $version + "-" + $suffix
        $documents = @('README.md', 'USAGE.md', 'ACCURACY.md', 'LICENSE', 'LICENSE-RUST-MIT', 'LICENSE-RUST-APACHE', 'THIRD-PARTY-LICENSES.md')

        # 同梱物が欠けたまま配布しないよう、先に存在確認する。
        $missing = @()
        foreach ($doc in $documents) {
            if (-not (Test-Path (Join-Path $RepoRoot $doc))) {
                $missing += $doc
            }
        }
        if ($missing.Count -gt 0) {
            Fail ("zip に同梱するファイルが見つかりません: " + ($missing -join ', '))
        }

        $stageRoot = Join-Path $outPath (".stage-" + $packageName)
        $stage = Join-Path $stageRoot $packageName
        $zipPath = Join-Path $outPath ($packageName + ".zip")
        $zipHashPath = $zipPath + ".sha256"
        try {
            if (Test-Path $stageRoot) {
                Remove-Item -Path $stageRoot -Recurse -Force
            }
            New-Item -ItemType Directory -Path $stage -Force | Out-Null

            # 実行ファイル (Debug 時は dskr-debug.exe)
            Copy-Item -Path $builtExe -Destination (Join-Path $stage $plainName) -Force

            foreach ($doc in $documents) {
                Copy-Item -Path (Join-Path $RepoRoot $doc) -Destination (Join-Path $stage $doc) -Force
            }

            # sha256sum 互換 ("<hash>  <file>")。BOM と CRLF を避けて ASCII/LF で書く。
            $exeHash = (Get-FileHash -Path (Join-Path $stage $plainName) -Algorithm SHA256).Hash.ToLowerInvariant()
            $sumsPath = Join-Path $stage 'SHA256SUMS.txt'
            $sumsText = $exeHash + '  ' + $plainName + "`n"
            [System.IO.File]::WriteAllText($sumsPath, $sumsText, [System.Text.Encoding]::ASCII)

            if (Test-Path $zipPath) {
                Remove-Item -Path $zipPath -Force
            }
            Compress-Archive -Path (Join-Path $stageRoot '*') -DestinationPath $zipPath -CompressionLevel Optimal

            $zipHash = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
            $zipHashText = $zipHash + '  ' + (Split-Path -Leaf $zipPath) + "`n"
            [System.IO.File]::WriteAllText($zipHashPath, $zipHashText, [System.Text.Encoding]::ASCII)
        } finally {
            if (Test-Path $stageRoot) {
                Remove-Item -Path $stageRoot -Recurse -Force
            }
        }

        Write-Info ("zip: " + $zipPath)
        Write-Info ("zip SHA-256 : " + $zipHash)
        Write-Info ("ハッシュファイル: " + $zipHashPath)
        Write-Info '同梱ファイル:'
        $packageFiles = @($plainName) + $documents + @('SHA256SUMS.txt')
        foreach ($name in $packageFiles) {
            Write-Info ("  - " + $name)
        }
    }

    # ------------------------------------------------------------------
    # 7. 結果表示
    # ------------------------------------------------------------------
    Write-Step '完了'
    $sizeMB = [math]::Round(((Get-Item $plainExe).Length / 1MB), 2)
    Write-Host ("  実行ファイル : " + $plainExe + " (" + $sizeMB + " MB)")
    Write-Host ("  バージョン付 : " + $versionedExe)
    if ($Zip) {
        Write-Host ("  zip          : " + $zipPath)
        Write-Host ("  zip SHA-256  : " + $zipHash)
    }
    Write-Host ("  SHA-256      : " + $hash)
    $elapsed = (Get-Date) - $script:Started
    Write-Host ("  所要時間     : " + [int]$elapsed.TotalSeconds + " 秒")
    Write-Host ''
    Write-Host '手動確認の推奨項目:' -ForegroundColor Gray
    Write-Host '  - dskr C:\Users などで TUI が起動し、q で戻ること' -ForegroundColor Gray
    Write-Host '  - UNC: dskr \\server\share --no-tui --top 5' -ForegroundColor Gray
    Write-Host '  - Ctrl+C / q の後に端末が通常状態へ戻ること' -ForegroundColor Gray
    Write-Host '  - 長いパス (>260 文字) とジャンクションを含むフォルダでクラッシュしないこと' -ForegroundColor Gray
    exit 0
}
catch {
    $message = $_.Exception.Message
    $detail = $script:LastCargoOutput
    Write-Host ''
    Write-Host ("エラー: " + $message) -ForegroundColor Red

    if ($message -match 'link|linker|LNK[0-9]+' -or $detail -match 'link\.exe|LNK[0-9]{4}|linker') {
        Write-Host ''
        Write-Host 'ヒント: MSVC リンカが見つからない可能性があります。' -ForegroundColor Yellow
        Write-Host '  Visual Studio Build Tools 2022 の「C++ によるデスクトップ開発」を導入してください。' -ForegroundColor Yellow
        Write-Host '  https://visualstudio.microsoft.com/ja/downloads/' -ForegroundColor Yellow
        Write-Host '  「x64 Native Tools Command Prompt for VS 2022」から実行すると確実です。' -ForegroundColor Yellow
    }
    if ($detail -match 'Cargo\.lock|--locked') {
        Write-Host ''
        Write-Host 'ヒント: Cargo.lock が更新されていない可能性があります。' -ForegroundColor Yellow
        Write-Host '  ネットワークのある環境で cargo generate-lockfile を実行し、Cargo.lock をコピーしてください。' -ForegroundColor Yellow
    }
    if ($message -match 'rustup|toolchain|not installed|offline' -or $detail -match 'offline|no matching package') {
        Write-Host ''
        Write-Host 'ヒント: rustup default stable-x86_64-pc-windows-msvc' -ForegroundColor Yellow
        Write-Host '       オフラインの場合は -Offline と vendor ディレクトリを確認してください。' -ForegroundColor Yellow
    }
    exit 1
}
