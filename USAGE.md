# dskr 使い方ガイド

`dskr` は、指定したディレクトリ配下のディスク使用量を高速に調べるツールです。この文書は利用者向けの詳細ガイドです。概要やビルド方法は [README.md](README.md) を参照してください。

- 対象バージョン: 0.1.0
- 対象 OS: Windows / macOS / Linux
- `dskr --help` は、実行する OS に合わせた説明と例を表示します

> この文書の対応表は Linux を「Linux / Unix」列の基準にしています。その他の Unix では一部の機能が制限されます。備考も合わせて確認してください。

## 1. 起動と基本操作

```text
dskr [OPTIONS] [PATH]
```

`PATH` を省略するとカレントディレクトリを走査します。

```sh
dskr /var                         # TUI で探索
dskr                              # カレントディレクトリを TUI で探索
dskr /var --no-tui                # 概要レポートを標準出力
dskr /var --format tree --depth 2 # フォルダ構成
dskr /var --allow-delete          # TUI で削除も可能にする
```

### TUI と画面なし出力

- 既定は TUI です
- `--no-tui` を指定すると概要レポートを標準出力します
- `--format` / `--output` / `--json` / `--csv` / `--top` を指定した場合も、画面なしで出力します
- `--allow-delete` は TUI 専用です。画面なし出力と併用するとエラーになります

### 終了コード

| コード | 意味 |
|---:|---|
| `0` | 正常終了 |
| `1` | パス・出力などの実行時エラー |
| `2` | コマンドライン引数のエラー（clap の使用方法エラー） |
| `130` | 利用者による中止（TUI の `Esc` / 強制終了など） |

## 2. オプション対応マトリクス

記号の意味:

- `○`: その OS で利用可能
- `△`: 利用可能だが制限・警告あり
- `—`: 非対応、または指定しても効果なし（受け付けても無視・警告）

### 2.1 走査・負荷オプション

| オプション | 内容 | Windows | macOS | Linux / Unix | 備考 |
|---|---|:---:|:---:|:---:|---|
| `PATH` | 走査対象ディレクトリ | ○ | ○ | ○ | Windows は UNC・拡張長パスに対応 |
| `-l`, `--load` | 負荷プリセット | ○ | ○ | ○ | `low` の I/O 制御は OS 依存。`max` はスレッド数を通常の2倍にする |
| `-p`, `--profile` | ローカル / ネットワーク判定 | ○ | △ | △ | `auto` の UNC 自動判定は Windows のみ。Unix は `network` を明示 |
| `-j`, `--threads` | 走査ワーカースレッド数 | ○ | ○ | ○ | ネットワーク時は未指定なら自動調整 |
| `--nice` | nice 値 (0-19) | — | ○ | ○ | Windows では効果なし（ヘルプ非表示・指定時警告） |
| `--io-priority` | I/O 優先度 | ○ | △ | △ | Windows: プロセス優先度 + EcoQoS / macOS: 未対応 / Linux: ioprio |
| `--max-entries-per-sec` | 1秒あたりの処理数上限 | ○ | ○ | ○ | 全ワーカー合計。最小 50 |
| `--max-nodes` | 保持ノード数の上限 | ○ | ○ | ○ | 既定 5,000,000。`0` は実質無制限 |
| `-m`, `--min-size` | 最小ファイルサイズ | ○ | ○ | ○ | 例: `10M` |
| `-D`, `--max-depth` | 最大深さ | ○ | ○ | ○ | ルート = 0 |
| `-L`, `--follow-links` | リンクを辿る | ○ | ○ | ○ | Windows は同一ボリューム内のみ。Unix はループ検出あり |
| `--local-only` | ネットワークを走査しない | ○ | ○ | △ | Windows: UNC/ネットワークドライブ/別ボリューム。Linux は主要ネットワークFSに対応 |
| `-x`, `--one-file-system` | ファイルシステム境界を越えない | ○ | ○ | ○ | |
| `-e`, `--exclude` | 除外パターン | ○ | ○ | ○ | ファイル名に対する `*` / `?` |
| `-H`, `--hidden` | 隠しファイル・システムファイルも表示 | ○ | ○ | ○ | 既定は非表示。Windows: HIDDEN/SYSTEM 属性、macOS: UF_HIDDEN、Unix: ドット始まり |
| `--skip-cloud-placeholders` | クラウドプレースホルダを除外 | ○ | — | — | Windows のみ（他OSではヘルプ非表示・指定時警告） |

### 2.2 出力オプション

| オプション | 内容 | Windows | macOS | Linux / Unix | 備考 |
|---|---|:---:|:---:|:---:|---|
| `--metric` | 合計に使うサイズ | ○ | ○ | ○ | `auto` はローカル=allocated、ネットワーク=logical |
| `-f`, `--format` | 出力形式 | ○ | ○ | ○ | 指定時は画面なし |
| `-o`, `--output` | 出力先 | ○ | ○ | ○ | `-` は標準出力 |
| `-d`, `--depth` | ツリーの深さ | ○ | ○ | ○ | ルート = 0 |
| `-s`, `--sort` | 並び順 | ○ | ○ | ○ | 既定 `size:desc` |
| `-u`, `--units` | 単位の基準 | ○ | ○ | ○ | IEC / SI |
| `-U`, `--unit` | 表示単位 | ○ | ○ | ○ | 既定 `auto` |
| `-a`, `--ascii` | ツリーの罫線を ASCII に | ○ | ○ | ○ | |
| `-C`, `--color` | 色付け | ○ | ○ | ○ | Windows の `auto` は Windows Terminal など色対応端末のみ |
| `-J`, `--json` | JSON の追加出力 | ○ | ○ | ○ | `--format json` の別名 |
| `-c`, `--csv` | CSV の追加出力 | ○ | ○ | ○ | `--format csv` の別名 |
| `-t`, `--top` | 上位 N 件 | ○ | ○ | ○ | `0` は無制限 |
| `-n`, `--no-tui` | 画面なし出力 | ○ | ○ | ○ | 既定は概要レポート |

主要なオプションには短縮形があります。例: `dskr /var -n -f tree -d 2`、`dskr /data -l low -p network`。

### 2.3 TUI・その他

| オプション | 内容 | Windows | macOS | Linux / Unix | 備考 |
|---|---|:---:|:---:|:---:|---|
| `--pause-on-start` | 一時停止状態で開始 | ○ | ○ | ○ | TUI 専用 |
| `--allow-delete` | TUI で削除を許可 | ○ | ○ | ○ | 確認付きの完全削除。TUI 専用 |
| `-h`, `--help` | ヘルプ | ○ | ○ | ○ | OS 別の内容を表示 |
| `-V`, `--version` | バージョン | ○ | ○ | ○ | |

### 2.4 OS 固有の機能・パス

| 機能 | Windows | macOS | Linux / Unix | 備考 |
|---|:---:|:---:|:---:|---|
| UNC パス `\\server\share` | ○ | — | — | Windows のみ |
| 拡張長パス `\\?\...` | ○ | — | — | Windows の長いパス対応 |
| ネットワークドライブの自動判定 | ○ | — | — | Unix は `--profile network` を指定 |
| ジャンクション / 再解析ポイント | ○ | — | — | Windows のみ |
| シンボリックリンク | ○ | ○ | ○ | |
| OneDrive プレースホルダの判定 | ○ | — | — | この実装では Windows のみ |
| ディスク上の使用量 (allocated) | ○ | ○ | ○ | Windows: NTFS 圧縮対応 / Unix: `st_blocks` |
| ハードリンクの重複排除 | ○ | ○ | ○ | Windows のネットワークプロファイルでは省略 |
| 260 文字超のパス | ○ | — | — | Windows のみ（他OSでは不要） |
| 色付け `auto` | ○ | ○ | ○ | Windows は Windows Terminal 判定あり |
| 完全削除 | ○ | ○ | ○ | ゴミ箱移動は非対応 |
| ゴミ箱へ移動 | — | — | — | `--allow-delete` でも完全削除 |

## 3. OS 別の注意点

### Windows

- `\\server\share` 形式の UNC パス、`Z:\` 形式のネットワークドライブに対応しています
- 長いパスは自動的に拡張長表記で走査します。明示する場合は `\\?\UNC\server\share\...` も使えます
- `--profile auto` は UNC / ネットワークドライブを自動判定します
- ジャンクション / シンボリックリンクは既定では辿りません。`--follow-links` 指定時も同一ボリューム内のみです
- 隠しファイルとシステムファイル（HIDDEN / SYSTEM 属性）は既定で一覧・集計から除外されます。表示するには `--hidden` を指定してください
- `--local-only` は UNC / ネットワークドライブに加え、ルートと異なるボリュームも走査しません
- OneDrive などのプレースホルダは、`--skip-cloud-placeholders` で集計から除外できます
- `--nice` は効果がないためヘルプに表示されません。指定すると警告を出して無視します
- `--io-priority` はプロセス優先度と EcoQoS を下げます
- 読み取り専用属性が付いたファイルは削除に失敗します。属性は変更しません
- `--color auto` は Windows Terminal など色対応端末のときだけ色付けします

### macOS

- `--profile auto` は通常ローカル扱いです。`/Volumes` などのネットワークマウントは `--profile network` を指定してください
- ドットファイルと `UF_HIDDEN` フラグ付きの項目は既定で一覧・集計から除外されます。表示するには `--hidden` を指定してください
- `--local-only` は NFS / SMB / WebDAV / AFP / FTP などのネットワークマウントを走査しません
- `--io-priority` は macOS では未対応です。指定すると警告を出し、nice のみ適用します
- 負荷を下げたい場合は `--load low` または `--nice` を使用してください
- `--skip-cloud-placeholders` はこの実装では Windows 専用のため、ヘルプに表示されません

### Linux / その他 Unix

- NFS / CIFS / SMB / FUSE / 9p などのネットワークFSを `--local-only` で除外できます
- WSL の `/mnt/c` は 9p として扱われるため、`--local-only` の対象になります
- `--io-priority` は Linux の x86_64 / aarch64 で ioprio を設定します。未対応アーキテクチャでは警告を出します
- Linux 以外の Unix では、ネットワークFSの自動判定や I/O 優先度が制限される場合があります
- `--profile auto` は通常ローカル扱いです。ネットワークマウントは `--profile network` を指定してください
- ドット始まりの隠しファイル・フォルダは既定で一覧・集計から除外されます。表示するには `--hidden` を指定してください
- Linux / Unix には Windows の SYSTEM 属性に相当する概念がないため、システムファイルの判定は行いません
- `--skip-cloud-placeholders` はこの実装では Windows 専用のため、ヘルプに表示されません

## 4. TUI 操作

### 基本キー

| キー | 動作 |
|---|---|
| `↑` / `↓` / `j` / `k` | 項目を移動 |
| `Enter` / `l` | フォルダを開く |
| `h` / `Backspace` / `←` | 一つ上へ |
| `g` / `G` | 先頭 / 末尾へ |
| `s` | 並び順の切替（サイズ → 名前 → ファイル数 → 更新日時） |
| `S` | 昇順 / 降順の反転 |
| `a` | 合計の種類（ディスク使用量 ⇔ 見かけのサイズ） |
| `u` | 表示単位の切替（自動 → B → KiB → MiB → …） |
| `U` | 単位の基準（IEC = 1024基準 ⇔ SI = 1000基準） |
| `/` | 名前で絞り込み（`Enter` 適用 / `Esc` 解除） |
| `E` | エラー一覧 |
| `e` | 結果を保存 |
| `d` | 選択項目を削除（`--allow-delete` 指定時のみ） |
| `?` | 全画面ヘルプ |
| `q` | 終了 |

### 走査中

| キー | 動作 |
|---|---|
| `Space` | 一時停止 / 再開 |
| `Esc` | 中止（途中までの結果を表示） |
| `q` | 中止して終了（`y` で強制終了） |

応答しないネットワーク先では中止に時間がかかることがあります。`Esc` で止まらない場合は `q` → `y` で強制終了してください。端末は元に戻ります。

### 絞り込み・並び順

- 既定は **サイズの大きい順** です
- `s` はサイズ → 名前 → ファイル数 → 更新日時 の順に切り替えます
- `S` で昇順 / 降順を反転します
- `/` で入力した文字列は、現在の階層の項目名に対する部分一致です

### 単位・合計の種類

- `a` は「ディスク使用量」と「見かけのサイズ」を切り替えます
- `u` は表示単位を切り替えます
- `U` は IEC（1024基準）と SI（1000基準）を切り替えます

### エラー一覧

`E` を押すと、権限不足などで走査できなかったパスとメッセージを表示します。`Esc` で閉じます。

### 結果の保存

`e` を押すと保存設定が開きます。

- `Tab` / `Shift+Tab`: 形式 → 件数 → 深さ → 保存 の移動
- `←` / `→`: 形式の変更
- 数字 / `↑` / `↓`: 件数・深さの指定
- `Enter`: 次の項目へ / 保存
- `Esc`: キャンセル

保存先はカレントディレクトリです。既存ファイルは上書きしません。

### 削除

`d` は `--allow-delete` 指定時だけ有効です。詳細は「削除機能」を参照してください。

## 5. 出力形式

| 形式 | 内容 | 既定の深さ / 件数 |
|---|---|---|
| `report` | 合計・容量の大きいフォルダ / ファイル・エラー | 上位10件 |
| `tree` | フォルダ構成（ルート比 %） | 深さ2 / 各階層20件 |
| `flat` | `サイズ 割合 パス` の一覧 | 上位50件 |
| `markdown` | 共有用 Markdown レポート | 深さ2 / 上位10件 |
| `html` | 単一ファイルの HTML | 深さ2 / 上位10件 |
| `json` | 機械処理用 JSON（ツリー全体） | 全件 |
| `csv` | 機械処理用 CSV（フラットな行） | 全件 |

```sh
dskr /var --no-tui
dskr /var --format tree --depth 2
dskr /var --format flat --top 50
dskr /var --format markdown --output report.md
dskr /var --format html --output report.html
dskr /var --no-tui --json out.json --csv out.csv
```

- `--output -` または省略時は標準出力です
- `--output` でファイルを指定した場合、既存ファイルは上書きされます。上書きしたくない場合は TUI の保存 (`e`) を使用してください
- `--format` / `--json` / `--csv` を指定すると、画面なしで出力します
- `--json` / `--csv` はファイル出力で、`--format json` / `--format csv` の別名です
- `--top 0` は無制限です。巨大な出力になる点に注意してください
- `--depth` はルートを 0 とした深さです
- `--format json` と `--json`、`--format csv` と `--csv` は同時に指定できません
- 一覧の並びは既定でサイズの大きい順です。`--sort name:asc` などで変更できます
- `--sort` は `tree` / `flat` の並びに適用されます
- `--ascii` でツリーの罫線を ASCII にできます
- `--color auto|always|never` で色付けを制御します
- 「走査: N 項目」の項目数は、隠しファイルの除外・`--min-size` / `--exclude` / `--max-depth` で集計から外した項目も含む「調べた項目数」です

JSON はツリー全体を、CSV はフラットな行を出力します。JSON / CSV は制御文字や表計算の数式を無害化します。JSON の `incomplete` は取得できなかった項目があること、`errors_truncated` はエラー一覧が上限 (1000件) を超えて省略されていることを示します。`allocated_collected` が false のとき、`allocated` は実際の割当サイズではありません。`mtime` は取得できない場合 `null` です。CSV の各行には、スキャン全体の状態として `cancelled` / `scan_truncated` / `scan_errors` / `allocated_collected` も入ります。

`-x` や `--local-only` で走査しないと決めたフォルダは、一覧では `(対象外)` と表示され、JSON の `"type"` は `"blocked"` になります (中身は集計されません)。

## 6. ネットワーク共有

### Windows

```sh
dskr \\fileserver\share\projects
dskr \\?\UNC\fileserver\share\data
dskr Z:\data
dskr \\fileserver\share --load low --max-entries-per-sec 500
```

- `--profile auto`（既定）は UNC / ネットワークドライブを検出します
- ネットワーク越しは「見かけのサイズ」を既定にします。`--metric allocated` で切り替えられます（Windows のネットワークプロファイルでは、`--metric allocated` を指定しない限り `a` キーでの切り替えはできません）
- ジャンクション / シンボリックリンクは既定では辿りません。`--follow-links` 指定時も同じボリューム内のみです
- 認証は OS の既存セッションのみ使用します。パスワードの入力・保存は行いません
- OneDrive などのプレースホルダはダウンロードされません。`--skip-cloud-placeholders` で集計から除外できます
- `--local-only` でネットワーク上の場所を走査しないようにできます
- **信頼できる共有のみ指定してください**

### macOS / Linux

```sh
dskr /Volumes/nas --profile network --load low     # macOS
dskr /mnt/nas --profile network --load low         # Linux
dskr /mnt/nas --profile network --local-only
```

- `--profile auto` は通常ローカル扱いです。NFS / SMB などのマウントでは `--profile network` を明示してください
- `--local-only` は OS が判定したネットワークFSへのアクセスを避けます
- ネットワーク越しの削除はサーバ側の権限に従います。応答が遅い場合は時間がかかることがあります

### リンクと `--local-only`

- `--follow-links` はシンボリックリンクを辿ります。ループ検出があります
- `-x` / `--local-only` 指定時は、別ファイルシステムやネットワークへのリンクを辿りません
- ルートのボリュームIDを取得できない場合、境界判定は安全側で走査を制限し、エラー一覧に記録します
- Windows では `--follow-links` 指定時も同一ボリューム内のリンク先のみ辿ります

## 7. 負荷制御

### `--load` プリセット

| プリセット | スレッド (local) | スレッド (network) | nice | I/O 優先度 | レート上限 |
|---|---|---|---|---|---|
| `low` | 論理CPU数 / 4（1〜8） | 基準の 1/4（2〜8） | +10 | idle | local: 無制限 / network: 2,000 項目/秒 |
| `normal` | 論理CPU数 | 8〜32 | なし | なし | 無制限 |
| `max` | 論理CPU数 × 2（2〜64） | 基準の 2 倍（16〜64） | なし | なし | 無制限 |

- ネットワークの基準スレッド数は `clamp(論理CPU数 × 4, 8, 32)` です。`low` / `max` はこの基準を分割・倍加します
- `max` はスレッド数を増やして SSD/NVMe や高レイテンシのネットワーク向けに振った設定です。HDD ではシーク競合で逆効果になる場合があります
- `--threads` はプリセットより優先され、1〜256 に制限されます
- `--max-entries-per-sec` はプリセットより優先され、最小 50 です
- 個別オプションで `--load` の内容を上書きできます

### OS ごとの違い

- Windows: `--nice` は効果がありません。`--io-priority` はプロセス優先度と EcoQoS を下げます
- macOS: `--io-priority` は未対応です（警告を出し、nice のみ適用）
- Linux: `--io-priority` は ioprio を設定します。対応アーキテクチャは x86_64 / aarch64 です
- その他の Unix: I/O 優先度が未対応の場合があります

```sh
dskr /data --load low
dskr /data --max-entries-per-sec 500 --threads 1
dskr /data --load low --max-entries-per-sec 500
```

`--pause-on-start` を使うと、TUI で確認してから走査を再開できます。

## 8. 削除機能（`--allow-delete`）

既定では削除できません。起動時に `--allow-delete` を指定したときだけ、TUI で `d` による削除が使えます。

```sh
dskr /data --allow-delete
```

### 操作

- 選択項目で `d` を押すと確認ダイアログが開きます
- ファイル / リンクは `y` で確定します
- フォルダは `yes` と入力して `Enter` で確定します
- `Esc` でキャンセルします

### 動作と制限

- 削除は**ゴミ箱ではなく完全削除**です。取り消せません
- フォルダは中身ごと削除されます
- シンボリックリンク / ジャンクションはリンク先を辿らず、リンク自体だけを削除します
- `--follow-links` で中身を辿ったリンクは、削除すると集計がずれるため削除できません。リンク先を直接開いて削除してください
- 削除に成功すると、画面の一覧と合計を即時更新します
- 削除できるのは、いま開いているフォルダの子項目だけです。走査ルート自体は削除できません
- 走査中は削除できません
- `--exclude` / `--min-size` / `--max-depth` / `--max-nodes` などで一覧・集計に出ていない項目も、フォルダ削除では一緒に削除されます
- 権限がないなどで一部だけ削除された場合はエラーを表示します。ツリーは自動で再走査しないため、必要なら dskr を起動し直してください
- Windows の読み取り専用属性が付いたファイルは削除に失敗します（属性は変更しません）
- ネットワーク共有上の削除はサーバ側の権限に従います
- `--no-tui` / `--format` / `--json` などの画面なし出力とは併用できません

> **注意**: 重要なデータは事前にバックアップしてください。

## 9. トラブルシューティング

### 結果が表示されない・少ない

- 権限不足の場合は `E` でエラー一覧を確認してください
- 割当サイズを取得できないファイルは、見かけのサイズで代替したうえでエラー一覧に記録されます
- 隠しファイル・システムファイルは既定で非表示です。必要な場合は `--hidden` を指定してください
- `--min-size` で小さいファイルを除外していないか確認してください
- `--max-depth` で深い階層を切っていないか確認してください
- `--max-nodes` の上限に達すると、子要素が省略され `truncated` と表示されます

### 走査が遅い・止まる

- ネットワークでは `--load low` や `--max-entries-per-sec 500` を試してください
- `Esc` で中止できます。応答しない場合は `q` → `y` で強制終了してください
- ネットワーク越しの `allocated` は時間がかかることがあります。`--metric logical` も検討してください

### Windows で長いパスを扱いたい

- dskr は長いパスを自動的に拡張長表記へ変換します
- 明示する場合は `\\?\UNC\server\share\...` などを使ってください

### OneDrive の項目を除外したい

- Windows では `--skip-cloud-placeholders` を指定してください
- macOS / Linux ではこのオプションは表示されません

### macOS で I/O 優先度の警告が出る

- macOS は `--io-priority` に未対応です
- `--nice` または `--load low` を使用してください

### Linux で I/O 優先度の警告が出る

- ioprio は x86_64 / aarch64 で対応しています
- それ以外のアーキテクチャでは `--nice` を使用してください

### 色が付かない

- `--color always` を指定してください
- Windows では Windows Terminal など色対応端末を使用してください

### 削除できない

- `--allow-delete` を付けて起動したか確認してください
- Windows の読み取り専用属性、権限、ネットワーク共有の権限を確認してください
- 一部だけ削除された可能性がある場合は、dskr を起動し直して最新の状態を確認してください

### ネットワークパスを指定できない

- UNC パスは Windows のみ対応です
- macOS / Linux ではマウント後のパス（`/Volumes/...`、`/mnt/...` など）を指定してください
- ネットワークマウントでは `--profile network` を指定してください

### `--allow-delete` でエラーになる

- `--allow-delete` は TUI 専用です
- `--no-tui` / `--format` / `--json` / `--csv` / `--output` / `--top` とは併用できません

## 10. セキュリティとプライバシー

- dskr はインターネットへ通信しません。外部への送信やテレメトリはありません
- 外部通信が起こり得るのは、UNC パス / ネットワークドライブを指定した場合の OS の SMB アクセスのみです
- 既定は読み取り専用です。`--allow-delete` を指定しない限り、削除・移動・属性変更は行いません
- 隠しファイル (`.ssh`、`.env` など) と Windows のシステムファイルは既定で一覧・合計・出力から除外されます。表示するには `--hidden` を指定してください
- `--allow-delete` 指定時の削除は、確認ダイアログを経た完全削除です
- ファイル名やエラーメッセージの制御文字は表示前に無害化します
- Markdown / HTML はファイル名をエスケープし、HTML は外部 CSS / JS / 画像を参照しません
- JSON / CSV は制御文字や表計算の数式を無害化します。Unix では 0600 で作成し、シンボリックリンクは辿りません
- TUI の `e` は保存設定で内容を確認してから保存し、既存ファイルを上書きしません

## 11. 付録：既定値一覧

| 項目 | 既定値 |
|---|---|
| `--load` | `normal` |
| `--profile` | `auto` |
| `--threads` | プリセット / ネットワーク時は自動調整 |
| `--nice` | なし |
| `--io-priority` | なし |
| `--max-entries-per-sec` | 無制限（`low` はネットワーク時のみ 2,000） |
| `--max-nodes` | 5,000,000（`0` は実質無制限） |
| `--min-size` | 0 |
| `--max-depth` | なし |
| `--follow-links` | 無効 |
| `--local-only` | 無効 |
| `-x`, `--one-file-system` | 無効 |
| `--exclude` | なし |
| `--hidden` | 無効（隠しファイル・システムファイルを非表示） |
| `--skip-cloud-placeholders` | 無効（Windows のみ表示） |
| `--metric` | `auto` |
| `--format` | なし（TUI。画面なし時は `report`） |
| `--output` | 標準出力 |
| `--depth` | 形式ごとの既定値 |
| `--sort` | `size:desc` |
| `--units` | `iec` |
| `--unit` | `auto` |
| `--ascii` | 無効 |
| `--color` | `auto` |
| `--json` / `--csv` | なし |
| `--top` | 形式ごとの既定値 |
| `--no-tui` | 無効 |
| `--pause-on-start` | 無効 |
| `--allow-delete` | 無効 |

## 関連ドキュメント

- [README.md](README.md): 概要、ビルド方法、開発者向け情報
- `dskr --help`: 実行中の OS に合わせたオプション一覧
