# AGENTS.md

`dskr` は TUI とヘッドレス出力を備えた単一バイナリの Rust 製ディスク使用量
エクスプローラ (Windows/macOS/Linux)。リポジトリ名・クレート名・
バイナリ名は `dskr`。

## コマンド

```sh
cargo build --release                                  # target/release/dskr
cargo test                                             # 単体 + 統合テスト
cargo test --lib                                       # 単体テストのみ
cargo test --test scan                                 # テストファイル単位
cargo test --test cli_output units_and_sort_can_be_selected   # テスト1件
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

CI は `fmt` → `clippy` を実行し (どちらも失敗でブロック)、Ubuntu / macOS /
Windows でテストする。Linux / macOS / Windows のパッケージ生成と検証も
CI で実行する。README にある `cargo check --target ...` は CI では
実行されない。

Windows 向けパッケージング: `scripts/build-windows.ps1` (`build-windows.cmd`
はラッパー)。`cargo build --locked` を使うため、`Cargo.lock` は最新の状態で
コミットされている必要がある。出力先 `dist/` は `target/` と同様に gitignore
対象。

Linux 向けパッケージング: `scripts/build-linux.sh --all` で静的 musl の
tar.gz / .deb / .rpm を作る。`rustup target add x86_64-unknown-linux-musl` と
`musl-tools` / `rpm` / `cpio` が必要。検証は
`scripts/verify-linux-packages.sh dist`。ダウンロード配布用で、公式
ディストリビューションリポジトリ向けではない。

macOS 向けパッケージング: `scripts/build-macos.sh --universal` で
aarch64 + x86_64 の universal2 tar.gz を作る。Xcode Command Line Tools と
`rustup target add aarch64-apple-darwin x86_64-apple-darwin` が必要。
検証は `scripts/verify-macos-packages.sh dist`。署名・公証は行わない。

## 守るべき制約

- **ネットワークコードとネットワーク系クレートを追加しない。** CI は
  `cargo tree` を `reqwest|hyper|ureq|curl|tokio|tonic|...` で、`objdump -T`
  を `connect|socket|getaddrinfo|...` で検査する。非同期ランタイムや HTTP
  クライアントを追加しないこと。
- **CLI オプションはテストで文書化が強制される。** `Cli` の long オプションが
  `USAGE.md` にないと `tests/docs.rs` が失敗する。`src/cli.rs` のテストと
  `tests/cli_output.rs::help_is_os_specific` は OS 別ヘルプの内容も検証する。
  オプションを追加・変更したら `USAGE.md` と該当する `cfg_attr` のヘルプを
  更新する。
- **利用者向け文字列・エラー・コメント・文書は日本語。** この方針を維持し、
  英語に翻訳しない。
- **リリースノート:** `.github/RELEASE_NOTES_TEMPLATE.md` の構成に従う。
  先頭に「対象環境」を置き、Windows のハンドルベース削除は
  Windows 11 / ローカル NTFS に限定されることを明記する。Windows 全般で
  安全に削除できると誤解されないよう、対象外の環境・操作も書く。
  `CHANGELOG.md` とコミット履歴から作成し、タグ push 後に Release ワークフローが
  作成したリリースへ `gh release edit --notes-file` で反映する。
- **0.5.x の削除安全制約:** 通常ディレクトリの再帰削除を行わない。
  `remove_dir_all` を使わない。削除できるのは空のディレクトリだけ。
  リンク経由の子孫、保護パス、UNC / ネットワーク / Unknown は削除しない。
  将来の再帰削除は「将来の再帰削除（0.6.0 以降）」節に従う。
- **既定値はテスト済みの仕様:** 隠し項目は `--hidden` 指定時のみ表示。
  ヘッドレスの既定形式は `report`。`--json` 単独でもヘッドレスになりファイル
  だけを書き出す。`--format json` と `--json` は併用不可。`--allow-delete` は
  TUI 専用。Markdown/HTML は外部参照のない単一ファイル。
- MSRV は 1.85 (`Cargo.toml` の `rust-version`)。それより新しい std API を
  使わない。

## アーキテクチャ

- `src/main.rs` — エントリポイント。引数解析、ヘッドレス/TUI の振り分け、
  終了コード (0 正常 / 1 実行時エラー / 2 clap の引数エラー / 130 中止) を
  担当。端末を復元するパニックフックも設定する。
- `src/lib.rs` — 統合テストが使う公開モジュール群。
- `src/cli.rs` — clap の定義と OS 別ヘルプ。
- `src/scan/engine.rs` — `crossbeam-channel` と `Control`/`Progress` による
  独自ワーカープール (一時停止・中止・レート制限)。rayon/tokio は使わない。
- `src/scan/node.rs` — アリーナ型ツリー。ノードは `u32` インデックスで参照
  するため、ノード数と `MAX_NODES_DEFAULT` の上限が重要。
- `src/scan/platform.rs` — Unix/Windows の割当サイズ、ハードリンク重複排除、
  ボリューム識別、優先度処理を `cfg` で分岐。
- `src/output/` — report/tree/flat/Markdown/HTML。`src/report.rs` —
  ストリーミング JSON/CSV。
- `src/app.rs` / `src/ui.rs` — TUI の状態と描画。
- `src/delete.rs` — 削除前のファイル ID 検証。`--allow-delete` 時のみ走査時に
  ID を収集し、削除直前に再取得して照合する。リンク先は辿らない。
  0.5.x では通常ディレクトリの再帰削除を行わず、空のディレクトリだけを削除する
  (`remove_dir_all` は使わない)。リンク経由の子孫は削除しない。保護パスは
  Exact / Subtree で判定し、削除しない。
  ネットワークプロファイル、Windows のネットワークパス、Unix/macOS の
  ネットワークマウントと Unknown のファイルシステムでは収集しない。
  Windows 11 ローカル NTFS の通常ファイルは同一ハンドルで ID・属性・
  ハードリンク数・ファイルシステムを再検証してから disposition を設定する。
  ファイルシンボリックリンク / ディレクトリシンボリックリンク / ジャンクションも、
  `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` でリンク自身の
  ハンドルを開き、ID・再解析タグ・DIRECTORY 属性・READONLY・ハードリンク数・
  NTFS を同じハンドルで検証してから disposition を設定する。対応タグは
  `IO_REPARSE_TAG_SYMLINK` と `IO_REPARSE_TAG_MOUNT_POINT` (DIRECTORY あり)
  だけで、失敗時に path-based 削除へフォールバックしない。
  disposition 成功後の `CloseHandle` だけで削除完了とせず、対象パスを
  `symlink_metadata` で再確認する。`NotFound` のみ成功とし、pending delete・
  置換・ID 取得失敗などは `Unknown` として扱う。

## 将来の再帰削除（0.6.0 以降）

0.5.x では通常ディレクトリの再帰削除を行わない。将来の再帰削除は、
単なる便利機能ではなく独立した安全性機構として設計する。0.5.x に
暫定的な再帰処理を追加してはいけない。

### 現行（0.5.x）の制約

- 削除できるのは空ディレクトリだけ。非空ディレクトリは拒否する。
- `remove_dir_all` と同等のパスベース再帰処理を使わない。
- リンク経由の子孫、UNC / Network / Unknown、保護パスは削除しない。
- 警告や確認ダイアログで制約を緩和しない。
- Windows 11 / ローカル NTFS の検証済み範囲を勝手に広げない。

### 再実装の原則

- 削除対象の探索・検証・削除を一体で設計する。
- OS が提供するディレクトリハンドル相対の操作を基本とする。
- `remove_dir_all()` の再導入や、同等のパスベース再帰処理の独自実装は
  行わない。
- OS ごとに実装を分け、共通化のために安全性の差を隠さない。
- マウント境界、リンク / reparse point、TOCTOU、削除対象の完全性、
  ハードリンク、部分削除を個別に解決する。
- スキャン時のツリー情報だけを信用しない。削除開始時に実際のディレクトリを
  再列挙し、スキャン後に追加された項目、隠し項目、除外項目、未知の種別、
  リンク、マウント、ID 変更を確認する。表示されていない項目を勝手に
  削除しない。
- マウント境界は削除開始地点のファイルシステムを基準とし、異なる FS、
  マウントポイント、bind mount を越えない。削除中にマウント状態が変化した
  場合の扱いを定義する。検証できない場合は拒否する。
- リンク先へ移動しない。リンク自身を削除する場合も、リンク先の実データを
  削除しない。未知の reparse tag は拒否する。
- FileId などの識別情報と親ディレクトリの識別情報を保持し、検査した対象と
  削除する対象が一致することを確認する。競合時は安全に中止する。
- ハードリンクは別パスとの共有を考慮し、リンク数と識別情報を確認する。
  ディレクトリ削除と通常ファイル削除の性質を区別する。
- 結果を「全件成功 / 拒否 / 失敗 / 一部削除 / 状態不明」に区別する。
  一部削除や状態不明を全件成功として表示しない。自動再試行せず、必要なら
  再スキャンを促す。

### OS 別の候補

- Linux: `openat2` (`RESOLVE_BENEATH` / `RESOLVE_NO_XDEV`)、`openat`、
  `fstatat`、`unlinkat`、ディレクトリ fd 相対操作、mount ID による境界判定。
- macOS: `openat`、`fstatat`、`unlinkat`、ディレクトリ fd 相対操作。
  Linux と同じ挙動を仮定せず、APFS などを実機検証する。
- Windows: ディレクトリハンドル相対の操作、FileId / VolumeSerialNumber の
  再検証、reparse point の識別、NTFS の削除挙動、共有モードと pending delete。
  既存の Windows 11 / ローカル NTFS の検証済み経路を、そのまま再帰削除へ
  拡張したとみなさない。Windows 10、ReFS、FAT / exFAT、UNC / ネットワークを
  無条件に対象へ拡張しない。

### 段階的対応

1. Linux の特定ファイルシステムで設計・実装・実機検証。
2. Linux の追加ファイルシステムやマウント条件を検証。
3. macOS で独立した実装・実機検証。
4. Windows で独立した実装・実機検証。
5. 対応 OS / ファイルシステムを README / USAGE / ACCURACY /
   リリースノートに明記する。

一部の OS で安全に動作しても、他の OS でも安全であると判断しない。

### 必須テスト（将来）

入れ子ディレクトリ、空ディレクトリ、隠し項目、除外項目、シンボリックリンク、
ディレクトリシンボリックリンク、ジャンクション、マウントポイント、
bind mount、ネストしたマウント、異なるファイルシステム、UNC / ネットワーク、
削除中のファイル追加、対象 / 祖先の置き換え競合、使用中ファイル、
アクセス拒否、部分削除、最終状態不明。削除対象外のファイル、リンク先、
マウント先が保持されていることも確認する。実機検証、CI 検証、クロスコンパイル
確認は区別して報告する。

### CLI

削除許可は `--allow-delete` を基本とする。`--allow-delete` は削除意思の明示で
あり、対象の安全性検証を省略するものではない。安全性条件を満たさない対象は
削除しない。再帰削除用の個別許可オプションは追加しない。ネットワーク削除は、
安全性を十分に証明できるまで対象外とする。

### フェーズ案

- R1: 仕様策定（境界、OS API、リンク / マウント、TOCTOU、状態管理、対応環境）
- R2: プロトタイプ（OS 別の小規模実装、競合 / マウントテスト）
- R3: 本実装（安全な OS から。空ディレクトリ削除・通常ファイル削除と統合）
- R4: 回帰・実機検証（OS 別、CI、既存機能）
- R5: 正式対応（ドキュメント、対応範囲、リリースノート）

バージョンや実施時期は未定。安全性を証明できない条件では削除を拒否する。

再帰削除は将来再実装するが、安全性を十分に設計・検証できた環境に限って
段階的に提供する。これを dskr の正式方針とする。

## テストの注意点

- テストは OS で分岐する (`#[cfg(unix)]`、`#[cfg(windows)]`、
  `#[cfg(target_os = "macos")]`)。ローカルの `cargo test` はホスト OS のみを
  対象とする。
- `tests/cli_output.rs` は `CARGO_BIN_EXE_dskr` 経由でビルド済みバイナリを
  起動する。テキスト検証を安定させるため `--color never` を渡す。
- `tests/scan.rs` は一時ディレクトリにフィクスチャを構築する。ハードリンク、
  レート制限、権限のテストはファイルシステムやタイミングに依存し、環境差が
  出やすい。
