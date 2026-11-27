# dskr — a fast file-size & disk-usage finder

高速なファイルサイズ・ディスク使用量ファインダー。指定ディレクトリ配下の
ファイル・フォルダがどれだけディスクを使っているかを高速に調べ、
ターミナルで確認できるツールです。Windows / macOS / Linux で動作する
単一バイナリとして配布できます。

> 詳細な使い方、OS ごとの対応表、トラブルシューティングは [USAGE.md](USAGE.md) を参照してください。サイズの数え方や `du` との違いは [ACCURACY.md](ACCURACY.md) にまとめています。`dskr --help` は実行する OS に合わせた内容を表示します。

```text
dskr レポート
対象       : /usr
種類       : ローカル
走査       : 30,538 項目 / 3,478 フォルダ / 1.60 秒 / 12 スレッド
合計       : 7.3 GiB (ディスク使用量) / 7.3 GiB (見かけのサイズ)
ファイル   : 30,538   フォルダ: 3,478   エラー: 0

容量の大きいフォルダ (10):
       6.8 GiB   93.6%       8134  /usr/lib
     230.2 MiB    3.1%       2210  /usr/share
     101.6 MiB    1.4%        412  /usr/bin
     ...
```

## 特徴

- **高速**: 複数のスレッドで並列に調べます。ファイルの中身は読みません (Windows のみ、ローカルのハードリンク判定に属性を 1 回読みます。ネットワークでは省略)
- **正確**: ディスク上の使用量と見かけのサイズの両方を集計
  - Unix: `st_blocks × 512` (スパースファイル対応)
  - Windows: `GetCompressedFileSizeW` (NTFS 圧縮対応)
  - 割当サイズを取得できない場合は見かけのサイズで代替し、エラー一覧に記録
  - ハードリンクは実体を1回だけ計上 (Windows のネットワークプロファイルを除く)
- **安全**: 既定は読み取り専用。`--allow-delete` を指定したときだけ、TUI で確認付きの削除ができます
- **隠し項目は既定で非表示**: ドットファイル (`.git`、`.env` など) や Windows のシステムファイルは、`--hidden` を指定したときだけ表示・集計します
- **外部送信なし**: ネットワーク通信コードを持たず、テレメトリも送信しません (UNC/ネットワークドライブを指定した場合のみ、OS の SMB アクセスが発生します)
- **負荷を抑えられる**: スレッド数・nice・I/O 優先度・1秒あたりの処理数上限を指定できます
- **単一バイナリ**: ランタイム不要。SSH 先でも動作
- **見やすい出力**: ターミナル用レポート / フォルダ構成 / `du` 風一覧 / Markdown / 単一 HTML

## ビルド

```sh
cargo build --release
# ./target/release/dskr
```

配布用のバイナリを作るときは、ビルド環境のローカルパスが混入しないよう
`--remap-path-prefix` を付けます。Windows / Linux / macOS のパッケージ
ビルドスクリプトは自動で設定します。

```sh
RUSTFLAGS="--remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/src" cargo build --release
```

### Windows 実機でのビルド

Windows 10/11 (64bit) にプロジェクトフォルダをコピーし、次を用意します。

1. **Rust (MSVC ツールチェーン)** — <https://rustup.rs> または `winget install Rustlang.Rustup`
2. **Visual Studio Build Tools 2022** の「C++ によるデスクトップ開発」

そのうえで、フォルダ直下の `build-windows.cmd` をダブルクリックするか、PowerShell から実行します。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Test -Zip
```

| オプション | 意味 |
|---|---|
| `-Test` | ビルド前に `cargo test --release` を実行 |
| `-Zip` | `dskr.exe` + README + USAGE + ACCURACY + LICENSE + Rust ライセンス + ハッシュを同梱した zip を `dist\` に作成 |
| `-Clean` | 事前に `cargo clean` |
| `-NoSmoke` | 生成した exe の起動確認を省略 |
| `-Offline` | `vendor/` を使ったオフラインビルド |
| `-OutDir <path>` | 出力先 (既定: `dist`) |
| `-Configuration Debug` | デバッグビルド |

出力:

```
dist\dskr.exe
dist\dskr-<version>-windows-x86_64.exe
dist\dskr-<version>-windows-x86_64.zip          (-Zip 指定時)
dist\dskr-<version>-windows-x86_64.zip.sha256   (-Zip 指定時)
```

zip の中身 (バージョン付きフォルダに同梱):

```text
dskr-<version>-windows-x86_64\
├── dskr.exe
├── README.md
├── USAGE.md
├── ACCURACY.md
├── LICENSE
├── LICENSE-RUST-MIT
├── LICENSE-RUST-APACHE
├── THIRD-PARTY-LICENSES.md
└── SHA256SUMS.txt
```

`THIRD-PARTY-LICENSES.md` には依存クレートのライセンスと著作権表示を記載しています
(`scripts/gen-licenses.sh` で生成。依存を更新したら再実行してコミットしてください)。
Linux の静的 musl パッケージには musl libc と Rust 標準ライブラリのライセンス通知
(`LICENSE-MUSL`、`LICENSE-RUST-MIT`、`LICENSE-RUST-APACHE`) を同梱します。
macOS と Windows のパッケージには Rust 標準ライブラリのライセンス通知を同梱します。

`SHA256SUMS.txt` は `dskr.exe` のハッシュを `sha256sum` 互換の形式で記録します。
`-Configuration Debug` では zip 名に `-debug` が付き、中の実行ファイルは `dskr-debug.exe` になります。

### Linux でのパッケージビルド

Linux 向けは静的 musl バイナリを 1 回ビルドし、tar.gz / .deb / .rpm を作成します。
ダウンロード配布用で、ディストリビューションの公式リポジトリ向けではありません。

```sh
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools rpm cpio
bash scripts/build-linux.sh --all
```

| オプション | 意味 |
|---|---|
| `--tar` | tar.gz と `.sha256` を作成（指定なしの既定） |
| `--deb` | `.deb` と `.sha256` を作成 |
| `--rpm` | `.rpm` と `.sha256` を作成 |
| `--all` | tar.gz / .deb / .rpm をすべて作成 |
| `--test` | ビルド前にホストで `cargo test --release` |
| `--clean` | 事前に `cargo clean` |
| `--no-smoke` | 起動確認を省略 |
| `--out-dir <path>` | 出力先（既定: `dist`） |
| `--target <triple>` | ターゲット（既定: `x86_64-unknown-linux-musl`） |

出力:

```text
dist/dskr-<version>-linux-x86_64.tar.gz
dist/dskr-<version>-linux-x86_64.tar.gz.sha256
dist/dskr_<version>-1_amd64.deb
dist/dskr_<version>-1_amd64.deb.sha256
dist/dskr-<version>-1.x86_64.rpm
dist/dskr-<version>-1.x86_64.rpm.sha256
```

tar.gz の中身:

```text
dskr-<version>-linux-x86_64/
├── dskr
├── README.md
├── USAGE.md
├── ACCURACY.md
├── LICENSE
├── LICENSE-MUSL
├── LICENSE-RUST-MIT
├── LICENSE-RUST-APACHE
├── THIRD-PARTY-LICENSES.md
└── SHA256SUMS.txt
```

.deb は `/usr/bin/dskr` と `/usr/share/doc/dskr/`、.rpm は `/usr/bin/dskr`、
`/usr/share/doc/dskr/`、`/usr/share/licenses/dskr/` に配置します。

```sh
sudo apt install ./dskr_0.5.0-1_amd64.deb
sudo dnf install ./dskr-0.5.0-1.x86_64.rpm
```

静的 musl のため glibc のバージョンに依存しません。musl libc と Rust 標準
ライブラリのライセンス通知を同梱しています。パッケージは未署名なので、
添付の SHA-256 を確認してから導入してください。

Linux パッケージの検証:

```sh
bash scripts/verify-linux-packages.sh dist
```

### macOS でのパッケージビルド

macOS では Apple Silicon と Intel を 1 つにまとめた universal2 の tar.gz を
作成します。Xcode Command Line Tools が必要です。ダウンロード配布用で、
App Store / 公証 (notarization) は行いません。

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
bash scripts/build-macos.sh --universal
```

| オプション | 意味 |
|---|---|
| `--universal` | aarch64 + x86_64 の universal2（既定） |
| `--arch aarch64\|x86_64` | 単一アーキテクチャ |
| `--test` | ビルド前に macOS 上で `cargo test --release` |
| `--clean` | 事前に `cargo clean` |
| `--no-smoke` | 起動確認を省略 |
| `--out-dir <path>` | 出力先（既定: `dist`） |

出力:

```text
dist/dskr-<version>-macos-universal2.tar.gz
dist/dskr-<version>-macos-universal2.tar.gz.sha256
```

tar.gz の中身:

```text
dskr-<version>-macos-universal2/
├── dskr
├── README.md
├── USAGE.md
├── ACCURACY.md
├── LICENSE
├── LICENSE-RUST-MIT
├── LICENSE-RUST-APACHE
├── THIRD-PARTY-LICENSES.md
└── SHA256SUMS.txt
```

`MACOSX_DEPLOYMENT_TARGET` の既定は `11.0` です。`lipo` で universal2 を作成し、
ad-hoc 署名を付け直します。これは Apple Silicon で実行するためのもので、
Gatekeeper の公証ではありません。

ブラウザからダウンロードした場合、quarantine 属性で起動を止められることがあります。

```sh
xattr -d com.apple.quarantine ./dskr
./dskr --version
```

macOS パッケージの検証:

```sh
bash scripts/verify-macos-packages.sh dist
```

### GitHub Releases での配布

`vX.Y.Z` のようなタグを push すると、GitHub Actions が Linux の tar.gz / .deb / .rpm、
macOS の universal2 tar.gz、Windows の zip を作成し、**すべて成功した場合だけ**
GitHub Releases に公開します (ワークフロー: `.github/workflows/release.yml`)。

```sh
git push origin main
git tag vX.Y.Z        # Cargo.toml の version と一致させること
git push origin vX.Y.Z
```

出力される成果物:

```text
dskr-<version>-linux-x86_64.tar.gz
dskr-<version>-linux-x86_64.tar.gz.sha256
dskr_<version>-1_amd64.deb
dskr_<version>-1_amd64.deb.sha256
dskr-<version>-1.x86_64.rpm
dskr-<version>-1.x86_64.rpm.sha256
dskr-<version>-macos-universal2.tar.gz
dskr-<version>-macos-universal2.tar.gz.sha256
dskr-<version>-windows-x86_64.zip
dskr-<version>-windows-x86_64.zip.sha256
```

タグ名と `Cargo.toml` の `version` が一致しない場合は失敗します。
再実行時は既存 Release に成果物を追加し直します。

Linux のビルドスクリプトはビルド後に `dskr --version` と
`dskr . --no-tui --max-depth 1 --top 5` で起動確認を行います。
macOS のビルドスクリプトは universal2 の `lipo -archs` と起動確認を行います。
Windows のスクリプトは `dskr . --no-tui --max-depth 1 --top 5` で起動確認を行い、
SHA-256 とサイズを表示します。
オフライン環境では、ネットワークのある環境で先に `cargo vendor vendor` を実行し、`.cargo\config.toml` に以下を追加してからフォルダごとコピーしてください。

```toml
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
```

> 実機での手動確認項目: UNC (`\\server\share`) の走査、長いパス (>260 文字)、ジャンクション、
> OneDrive プレースホルダ、NTFS 圧縮 / スパース / ハードリンク、ファイルロック中、
> 読み取り専用属性・ACL、Windows Terminal での Ctrl+C / `q` 後の端末復元。
> 配布時はコード署名 (SmartScreen 警告の低減) も検討してください。

## 使い方

### 画面で操作する (TUI)

```sh
dskr /var                 # TUI で探索 (既定)
dskr                      # カレントディレクトリ
```

| キー | 動作 |
|---|---|
| `↑↓ / j k` | 移動 |
| `Enter / l` | フォルダを開く |
| `h / BS` | 一つ上へ (左キーも可) |
| `g / G` | 先頭 / 末尾 |
| `s` | 並び順の切替 (サイズ → 名前 → ファイル数 → 更新日時) |
| `S` | 昇順 / 降順の反転 |
| `a` | 合計の種類 (ディスク使用量 ⇔ 見かけのサイズ) |
| `u` | 表示単位の切替 (自動→B→KiB→MiB→…) |
| `U` | 単位の基準 (IEC = 1024基準 ⇔ SI = 1000基準) |
| `/` | 名前で絞り込み (Enter 適用 / Esc 解除) |
| `E` | エラー一覧 (権限拒否など) |
| `e` | 結果を保存 (形式・件数・深さを指定) |
| `d` | 選択項目を削除 (`--allow-delete` 指定時のみ) |
| `?` | 全画面ヘルプ (`j/k`, `PgUp/PgDn` でスクロール) |
| `q` | 終了 |

既定は **サイズの大きい順** です。`S` を押すと昇順になります。
隠しファイル・システムファイルは既定では一覧にも合計にも含まれません。表示する場合は起動時に `--hidden` を指定してください。
画面が十分に広いときは更新日時 (`YYYY-MM-DD HH:MM`) も表示されます。
走査中は `Space` で一時停止/再開、`Esc` で中止 (途中までの結果をそのまま確認できます)。
応答しないネットワーク先などで中止が完了しない場合は `q` → `y` で強制終了できます。

`e` を押すと保存設定が開きます。`Tab` / `Shift+Tab` で「形式 → 件数 → 深さ → 保存」を移動し、`←/→` で形式、数字と `↑/↓` で件数・深さを指定します。形式ごとに使わない項目は自動で無効になります。保存先はカレントディレクトリで、既存ファイルは上書きしません。

保存できる形式は、概要レポート / フォルダ構成 / 一覧 / Markdown / HTML / JSON / CSV です。

#### 削除機能 (`--allow-delete`)

既定では削除できません。起動時に `--allow-delete` を指定したときだけ、TUI で `d` による削除が使えます。

```sh
dskr /data --allow-delete
```

- 選択項目で `d` を押すと確認ダイアログが開きます
- ファイル / リンクは `y`、フォルダは `yes` と入力して `Enter` で確定します
- 削除は**ゴミ箱ではなく完全削除**です。取り消せません
- フォルダは中身ごと削除されます
- シンボリックリンク / ジャンクションはリンク先を辿らず、リンク自体だけを削除します
- 削除直前に、走査時に記録したファイル ID (Unix: device / inode、Windows: volume serial / file index) とリンク種別を再取得して照合します。ID や種別が一致しない場合は削除しません
- ファイル ID を取得できない場合や、走査時と削除時のどちらかで取得できなかった場合も削除しません
- Windows のネットワークファイルシステムなど、ID を取得できない環境では削除できません
- フォルダはフォルダ自身の ID だけを検証し、中身は削除実行時点の状態を削除します。走査後に追加されたファイルも削除され、内部の全ファイルについて走査時の ID 一致を保証するものではありません
- `--follow-links` で中身を辿ったリンクは、削除すると集計がずれるため削除できません。リンク先を直接開いて削除してください
- 削除に成功すると、画面の一覧と合計を即時更新します
- ハードリンクの一方を削除しても、割当サイズは残存リンクへ引き継ぎます
- `--no-tui` / `--format` / `--json` などの画面なし出力とは併用できません

> **注意**: 削除は元に戻せません。重要なデータは事前にバックアップしてください。

削除の制限と失敗時の挙動:

- 削除できるのは、いま開いているフォルダの子項目だけです。走査ルート自体は削除できません
- 走査中は削除できません。走査完了後に `d` を押してください
- `--exclude` / `--min-size` / `--max-depth` / `--max-nodes` などで一覧・集計に出ていない項目も、フォルダ削除では一緒に削除されます
- 権限がないなどで一部だけ削除された場合はエラーを表示します。画面のツリーは自動で再走査しないため、必要なら dskr を起動し直してください
- Windows の読み取り専用属性が付いたファイルは、環境によって削除に失敗します (属性は変更しません)
- ネットワーク共有上の削除はサーバ側の権限に従います。応答が遅い場合は処理に時間がかかることがあります

### 画面を使わない出力

`--no-tui` を指定すると、既定で**概要レポート**を標準出力へ出します。

```sh
dskr /var --no-tui                          # 概要レポート
dskr /var --format tree --depth 2           # フォルダ構成
dskr /var --format flat --top 50            # du 風フラット一覧
dskr /var --format markdown --output report.md
dskr /var --format html --output report.html
dskr /var --no-tui --json out.json --csv out.csv   # 追加で JSON/CSV も保存
```

| 形式 | 内容 | 既定の深さ/件数 |
|---|---|---|
| `report` | 合計・容量の大きいフォルダ/ファイル・エラー | 上位10件 |
| `tree` | フォルダ構成 (ルート比%) | 深さ2 / 各階層20件 |
| `flat` | `サイズ  割合  パス` の一覧 | 上位50件 |
| `markdown` | 共有用 Markdown レポート | 深さ2 / 上位10件 |
| `html` | 単一ファイルの HTML (外部参照なし) | 深さ2 / 上位10件 |
| `json` / `csv` | 機械処理用 (従来形式) | 全件 |

- `--output -` または省略時は標準出力。`--json` / `--csv` はファイル出力
- `--format` / `--json` / `--csv` を指定すると**自動的に画面なしで出力**(`--no-tui` 不要)
- 出力の並びは既定で**サイズの大きい順**。`--sort name:asc` などで変更可能
- `--top 0` は無制限 (巨大な出力になる点に注意)
- `--depth` はルートを 0 とした深さ
- `--units iec|si` / `--unit auto|b|kib|mib|…` で表示単位を変更
- `--ascii` でフォルダ構成の罫線を ASCII に、`--color auto|always|never` で色付けを制御
- レポート/Markdown/HTML の一覧は「大きい順」で、`--sort` は tree/flat の並びに適用

> 補足: `dskr PATH --no-tui` の既定出力は概要レポートです。
> `--json` は `--no-tui` なしでも書き出されます。

## 主なオプション

OS ごとの対応状況や詳しい制限は [USAGE.md](USAGE.md) のマトリクスを参照してください。

| オプション | 説明 |
|---|---|
| `-l, --load low\|normal\|max` | 負荷の目安 (既定 normal) |
| `-p, --profile auto\|local\|network` | ローカル/ネットワークの判定 (既定 auto) |
| `-j, --threads N` | 同時に調べるスレッド数 |
| `--nice N` | nice 値 (Unix, 0-19) |
| `--io-priority idle\|low\|normal` | I/O 優先度 (Linux: ioprio, Windows: BELOW_NORMAL + EcoQoS。macOS は nice のみ) |
| `--max-entries-per-sec N` | 1秒あたりに調べる項目数の上限 (全ワーカー合計、最小50) |
| `-m, --min-size 10M` | 指定サイズ未満のファイルを集計しない |
| `-D, --max-depth N` | 指定より深い階層の項目を作らない |
| `--max-nodes N` | 保持する項目数の上限 (既定 5,000,000) |
| `-L, --follow-links` | シンボリックリンクを辿る (ループ検出あり。-x / local-only では辿らない) |
| `--local-only` | ネットワーク上の場所を一切走査しない (リンクも辿らない) |
| `-x, --one-file-system` | ファイルシステムの境界を越えない (リンクも辿らない) |
| `-e, --exclude GLOB` | 除外パターン (ファイル名に対する `*` / `?`) |
| `-H, --hidden` | 隠しファイル・システムファイルも表示 (既定: 非表示) |
| `--skip-cloud-placeholders` | OneDrive 等のプレースホルダを除外 (Windows) |
| `--metric auto\|allocated\|logical` | 合計に使うサイズ (allocated=ディスク上の使用量 / logical=見かけのサイズ) |
| `-f, --format report\|tree\|flat\|markdown\|html\|json\|csv` | 出力形式 |
| `-o, --output FILE\|-` | 出力先 (省略時は標準出力) |
| `-d, --depth N` | tree / markdown / html の深さ |
| `-t, --top N` | 表示する件数 (report/tree/flat/markdown/html。0 で無制限) |
| `-s, --sort size\|name\|count\|mtime[:asc\|desc]` | 並び順 (既定 size:desc) |
| `-u, --units iec\|si` | 1024 基準 / 1000 基準 |
| `-U, --unit auto\|b\|kib\|mib\|gib\|tib\|pib\|eib` | 表示単位 |
| `-a, --ascii` | フォルダ構成の罫線を ASCII に |
| `-C, --color auto\|always\|never` | 色付け |
| `-J, --json FILE` / `-c, --csv FILE` | JSON / CSV の追加出力 |
| `--pause-on-start` | 一時停止状態で開始 |
| `--allow-delete` | TUI で選択項目の削除を許可 (確認付きの完全削除。既定は無効) |

主要なオプションには短縮形があります。例: `dskr /var -n -f tree -d 2`、`dskr /data -l low -p network`。

## 負荷を抑えて使う

裏で走らせて PC を重くしたくない場合は:

```sh
dskr /data --load low              # スレッド CPU/4 (最大8) + nice +10 + I/O idle
dskr /data --max-entries-per-sec 500 --threads 1
```

`--load low` のレート上限はローカルでは無制限、ネットワークのみ 2,000 項目/秒です。
スレッド数・nice・I/O 優先度・レート上限は個別オプションで上書きできます。
`--pause-on-start` で TUI で確認してから開始することも可能です。

## UNC / ネットワーク共有 (Windows)

```sh
dskr \\fileserver\share\projects          # 標準 UNC
dskr \\?\UNC\fileserver\share\data        # 拡張長パス (長パス対応)
dskr Z:\data                              # ネットワークドライブも自動判定
dskr \\fileserver\share --load low --max-entries-per-sec 500
```

- `--profile auto` (既定) では UNC / ネットワークドライブを検出し、ネットワーク向けの設定で動作します
- ネットワーク越しはサーバの対応状況が異なるため、既定で「見かけのサイズ」を表示します (`a` キーで切替)。Windows のネットワークプロファイルでは割当サイズを取得しないため、`--metric allocated` を指定しない限り `a` で切り替えられません
- ジャンクション・シンボリックリンクは既定で辿りません (`--follow-links` 指定時も、同じボリューム内のみ)
- 認証は OS の既存セッションのみ使用します。パスワードの入力・保存は行いません
- OneDrive 等のクラウド プレースホルダはダウンロードされません。`--skip-cloud-placeholders` で集計から除外できます
- 応答しないサーバーでは、中止に時間がかかることがあります。`Esc` で止まらない場合は `q` → `y` で強制終了してください (画面は元に戻ります)
- ネットワーク上の場所を一切走査したくない場合は `--local-only` を指定してください
- **信頼できる共有のみ指定してください**。接続自体は OS の認証に従います

## プライバシーとセキュリティ

- 本ツールはインターネットへ通信しません。外部への送信やテレメトリはありません
- 隠しファイル (`.ssh`、`.env` など) と Windows のシステムファイルは既定で一覧・合計・出力から除外されます。表示するには `--hidden` を指定してください
- 外部通信が起こり得るのは、UNC パス/ネットワークドライブを指定した場合の OS の SMB アクセスのみです
- `--local-only` を指定すると、ネットワーク上の場所 (NFS, SMB, sshfs 等) を走査せず、リンクも辿りません
- `--follow-links` 指定時も `-x` / `--local-only` ではリンクを辿りません。Windows では同じボリューム内のリンク先のみ辿ります
- ルートのボリュームIDを取得できない場合、`-x` / `--local-only` の境界判定は安全側で走査を制限し、エラー一覧に記録します
- 既定は読み取り専用です。`--allow-delete` を指定しない限り、削除・移動・属性変更は行わず、ファイルの中身も開きません
- `--allow-delete` 指定時の削除は確認ダイアログを経た完全削除です。フォルダは中身ごと削除され、リンクはリンク先を辿りません
- ファイル名やエラーメッセージの制御文字は表示前に無害化します (端末制御やクリップボード操作を防ぐため)
- Markdown/HTML はファイル名をエスケープし、HTML は外部 CSS/JS/画像を参照しない単一ファイルです
- JSON/CSV は制御文字や表計算の数式を無害化します。Unix では 0600 で作成し、シンボリックリンクは辿りません
- TUI の `e` キーは保存設定で形式・件数・深さを確認してから保存し、既存ファイルを上書きしません

## レポート形式

### 人間向け
- `report`: 合計と容量の大きいフォルダ/ファイル、エラーのサマリー
- `tree`: フォルダ構成 (サイズ・ルート比%)
- `flat`: `サイズ 割合 パス` の一覧
- `markdown`: 表と折りたたみのフォルダ構成を含む共有用レポート
- `html`: 単一ファイル。外部参照なし。表はクリックで並べ替え可能

### 機械処理用
JSON (ツリー全体) と CSV (フラットな行) を出力します。

```json
{
  "schema_version": 1,
  "dskr_version": "0.5.0",
  "generated_at": "2026-11-25T00:30:04.357Z",
  "root": "\\\\fileserver\\share",
  "root_kind": "unc",
  "profile": "network",
  "metric": "logical",
  "allocated_collected": false,
  "cancelled": false,
  "truncated": false,
  "incomplete": true,
  "errors_truncated": false,
  "blocked_folders": 0,
  "stats": { "entries": 4, "dirs": 3, "errors": 1, "elapsed_ms": 51, "threads": 12 },
  "options": { "load": "normal", "...": "..." },
  "errors": [ { "path": "...", "message": "..." } ],
  "tree": { "name": "…", "type": "dir", "allocated": 0, "logical": 0, "files": 0, "dirs": 0, "children": [ … ] }
}
```

`tree` の `type` は `dir` / `file` / `other` (リンク・特殊ファイル) / `blocked` (`-x` や `--local-only` で走査しないフォルダ。中身は集計されない) のいずれかです。

`incomplete` は、権限不足や列挙中エラーなどで取得できなかった項目があることを示します。`errors_truncated` は、エラー一覧が上限 (1000件) を超えて省略されていることを示します。`allocated_collected` が false のとき、`allocated` は実際の割当サイズではありません (Windows のネットワークプロファイルなど)。`mtime` は取得できない場合 `null` になります。`schema_version` は JSON / CSV の公開スキーマのバージョン、`blocked_folders` は `-x` / `--local-only` で対象外にしたフォルダ数です。

CSV: `path,is_dir,allocated,logical,file_count,dir_count,error_count,truncated,cancelled,scan_truncated,scan_errors,allocated_collected,scan_blocked,schema_version`
- `scan_blocked` はスキャン全体の対象外フォルダ数、`schema_version` は公開スキーマのバージョンです。
- パス区切りは実行 OS のネイティブ表記です。

## パフォーマンス

| 環境 | 結果 |
|---|---|
| WSL2 (ext4, 12 スレッド) | 30,538 ファイル / 1.6 秒 (同一環境の `du -s` は 6.2 秒) |
| 1,000,000 空ファイル (warm cache, Linux) | dskr 約 0.51 秒 / 最大 RSS 約 148 MiB / キャンセル応答 約 6 ms (`du -s` は約 1.18 秒) |
| 負荷プリセット `low` | スレッドを CPU/4（最大8）に削減。レート上限はネットワークのみ 2,000 項目/秒 |

### 2026-11-27 の比較測定

条件: Linux WSL2 / Intel Core i5-11400H / `/dev/sdd` / warm cache /
3 回の中央値 / 1 ディレクトリあたり 100 または 1,000 ファイル /
深さ 2 / ハードリンクなし。dskr は `0.4.0` (`c6a2853`)、
gdu `5.25.0`、dua `3.0.3`、ncdu `1.22`、du (GNU coreutils) `9.7`。

| データ | ツール | wall (ms) | 最大 RSS (MiB) | CPU (s) |
|---|---|---:|---:|---:|
| 10,000 × 1 KiB | dskr | 61 | 5.4 | 0.02 |
| | du | 21 | 2.1 | 0.00 |
| | gdu | 42 | 8.8 | 0.02 |
| | dua | 53 | 3.7 | 0.07 |
| | ncdu | 22 | 2.5 | 0.00 |
| 100,000 × 1 KiB | dskr | 92 | 18.4 | 0.28 |
| | du | 125 | 2.3 | 0.09 |
| | gdu | 79 | 49.8 | 0.28 |
| | dua | 447 | 4.3 | 1.12 |
| | ncdu | 137 | 2.5 | 0.10 |
| 1,000,000 × 0 B | dskr | 510 | 148.1 | 2.82 |
| | du | 1,176 | 2.5 | 1.15 |
| | gdu | 392 | 621.6 | 3.47 |
| | dua | 4,354 | 4.2 | 10.90 |
| | ncdu | 1,216 | 2.6 | 1.17 |

dskr の中止応答 (SIGINT 送信から終了まで): 10k 1 ms / 100k 1 ms /
1M 6 ms。1M の JSON 出力は 502 ms、CSV 出力は 597 ms (中央値) でした。

CSV のパス構築を再利用バッファに変更した効果 (同一バイナリ条件、
warm cache、3〜5 回の中央値):

| データ | 変更前 | 変更後 |
|---|---:|---:|
| 深さ 1,000 / 10,000 ファイル | 139 ms | 124 ms |
| 100,000 ファイル (フラット) | 101 ms | 88 ms |

注意:

- cold cache は root 権限が必要なため未実施です。`sudo env BENCH_DROP_CACHES=1 scripts/bench.sh` で実行できます。
- ツールごとに既定のサイズ計測方式が異なります。dskr は allocated と logical の両方を持ち、du / gdu / dua / ncdu の既定は割当サイズです。dua は `-A`、ncdu は `--apparent-size` で見かけのサイズになります。
- ncdu はスキャンと export 生成、gdu / dua は一覧出力を含む時間です。dskr は JSON / CSV の出力を含みます。ツール間の優劣を断定せず、同一条件の参考値として扱ってください。

ネイティブ SSD 環境では並列 `stat` によりさらに高速に動作します。ボトルネックはディスクのメタデータ I/O であり、CPU ではありません。

### ベンチマーク

再現可能なフィクスチャで wall time / 最大 RSS / CPU / I/O を測るスクリプトを同梱しています。

```sh
cargo build --release
scripts/bench.sh
```

既定では 10,000 / 100,000 ファイルを生成し、各ケースを3回測ります。`du -s` も比較対象として実行します。

```sh
# 大規模データを測る (ディスク容量に注意)
BENCH_SIZES="1000000" BENCH_FILES_PER_DIR=1000 scripts/bench.sh

# 100ms 後に SIGINT を送り、中止までの応答時間も測る
BENCH_SIZES="1000000" BENCH_CANCEL_AFTER_MS=100 scripts/bench.sh

# gdu / dua / ncdu も比較する (事前に各自インストールしたものだけを使う)
BENCH_TOOLS="gdu dua ncdu" scripts/bench.sh

# フィクスチャを消す / コールドキャッシュで測る
BENCH_KEEP=0 scripts/bench.sh
sudo env BENCH_DROP_CACHES=1 scripts/bench.sh
```

主な環境変数:

| 変数 | 既定値 | 意味 |
|---|---|---|
| `BENCH_SIZES` | `10000 100000` | ファイル数の一覧 |
| `BENCH_FILES_PER_DIR` | `100` | 1フォルダあたりのファイル数 |
| `BENCH_FILE_SIZE` | `1024` | 各ファイルのサイズ (バイト) |
| `BENCH_REPEATS` | `3` | 計測回数 |
| `BENCH_WARMUP` | `1` | ウォームアップ回数 |
| `BENCH_BASE` | `$TMPDIR/dskr-bench` | フィクスチャ置き場 |
| `BENCH_KEEP` | `1` | フィクスチャを残す |
| `BENCH_DROP_CACHES` | `0` | Linux でページキャッシュを落とす (root) |
| `BENCH_DU` | `1` | `du -s` も計測する |
| `BENCH_CANCEL_AFTER_MS` | `0` | 指定ミリ秒後に SIGINT を送り、中止応答を測る |
| `BENCH_TOOLS` | 空 | 比較する外部ツール (`gdu dua ncdu`) |
| `BENCH_GDU_CMD` | `gdu -n` | gdu のコマンド |
| `BENCH_DUA_CMD` | `dua aggregate` | dua のコマンド |
| `BENCH_NCDU_CMD` | `ncdu -o` | ncdu の非対話コマンド |

出力は TSV です。先頭に `#` コメントで OS・CPU・スレッド数・メモリ・ストレージ・ファイルシステム・キャッシュ状態・フィクスチャの形 (深さ、空ファイル比率、ハードリンク有無)・dskr のバージョンとコミットを記録します。Linux では `/proc` から RSS と I/O を取得し、macOS では `ps` から RSS/CPU を取得します (I/O は未対応)。Windows は WSL など bash が動く環境で実行してください。`-cancel` 行の `wall_ms` は、SIGINT を送ってから終了するまでの時間です。

gdu / dua / ncdu は同梱せず、自動インストールもしません。利用者が導入した実行ファイルだけを任意で呼び出し、未導入ならスキップします。結果は同一条件での参考値として扱い、ツール間の優劣を断定しないでください。

結果は環境依存です。比較するときは、同じハードウェア・同じファイル構成・同じキャッシュ状態で測ってください。

## 開発

```sh
cargo test            # 単体 + 統合テスト
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo check --target x86_64-pc-windows-gnu   # Windows クロスコンパイル確認
cargo check --target aarch64-apple-darwin    # macOS クロスコンパイル確認
```

通信コードが存在しないことは次のように検証できます。

```sh
# ネットワーク関連システムコールの実測 (AF_INET の socket/connect が 0 件であること)
strace -f -e trace=%network ./target/release/dskr /path --no-tui
# 動的インポートシンボルに通信 API がないこと
# (socketpair/sendmsg/recvmsg は crossterm の自己パイプ用)
objdump -T target/release/dskr | grep -E '(^|[[:space:]])(connect|getaddrinfo|sendto|bind|listen|accept|socket)$'
```

CI (GitHub Actions) は Linux / macOS / Windows のマトリクスでテストし、MSRV 1.85 のジョブも実行します。Windows ランナーでは `\\localhost\C$` を使った UNC スモークテストを任意ジョブとして実行し、存在しない共有のエラーハンドリングは必須ジョブとして検証します。

構成:

```
src/
├── main.rs          # エントリ、CLI と TUI/ヘッドレスの振り分け
├── cli.rs           # clap による引数定義
├── delete.rs        # 削除前のファイル ID 検証 (リンク先は辿らない)
├── path.rs          # パス種別判定 (UNC)、glob、表示短縮
├── format.rs        # サイズ単位 (IEC/SI・自動/固定)・件数の表記
├── sort.rs          # ソート種類/方向の共通ロジック
├── scan/
│   ├── engine.rs    # 並列走査、負荷制御 (一時停止/レート制限/優先度)
│   ├── node.rs      # アリーナ型ツリーと集計
│   └── platform.rs  # 割当サイズ、ハードリンク判定、優先度適用 (OS別)
├── app.rs           # TUI 状態
├── ui.rs            # TUI 描画(列ヘッダー/フッター/ヘルプ)
├── output/          # report / tree / flat / Markdown / HTML
└── report.rs        # JSON/CSV エクスポート
```

## Author

QQK (<https://github.com/QuunQuickKick>)

## ライセンス

MIT License. 詳細は [LICENSE](LICENSE) を参照してください。

Copyright (c) 2026 QQK
