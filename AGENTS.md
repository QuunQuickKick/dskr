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

## テストの注意点

- テストは OS で分岐する (`#[cfg(unix)]`、`#[cfg(windows)]`、
  `#[cfg(target_os = "macos")]`)。ローカルの `cargo test` はホスト OS のみを
  対象とする。
- `tests/cli_output.rs` は `CARGO_BIN_EXE_dskr` 経由でビルド済みバイナリを
  起動する。テキスト検証を安定させるため `--color never` を渡す。
- `tests/scan.rs` は一時ディレクトリにフィクスチャを構築する。ハードリンク、
  レート制限、権限のテストはファイルシステムやタイミングに依存し、環境差が
  出やすい。
