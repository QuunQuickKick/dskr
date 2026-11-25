#!/usr/bin/env bash
# 依存クレートの第三者ライセンス一覧 (THIRD-PARTY-LICENSES.md) を生成する。
#
# - ci.yml (lint ジョブ) で生成結果とコミット済みファイルの差分を検査する
#   (依存を更新したら本スクリプトを再実行してコミットすること)
# - release-windows.yml でタグビルド前に実行し、配布 zip に同梱する
#
# ライセンス本文は third_party/licenses/ 以下に固定で同梱している。
# 新しいライセンス種別の依存を追加した場合は本文ファイルの追加が必要
# (表のライセンス列に現れた種別に対応すること)。
set -euo pipefail

cd "$(dirname "$0")/.."

OUT="THIRD-PARTY-LICENSES.md"

# cargo metadata は Cargo.lock 上の全パッケージ (他ターゲット向けを含む) の
# 取得に必要なため、オフラインを指定しない (CI ではネットワークが使える)。
meta=$(cargo metadata --format-version 1)

# packages 配列だけを切り出し、1 行 1 パッケージへ分割する。
# パッケージの境界は `},{"name":"…","version":` という並びでのみ現れる
# (features や targets の内側には name+version の組は現れない)。
packages=$(printf '%s' "$meta" \
  | sed -e 's/.*"packages":\[//' -e 's/\],"workspace_members".*//' \
    -e 's/},{"name":"\([^"]*\)","version":"/}\n{"name":"\1","version":"/g')

rows=""
used_licenses=""
count=0
while IFS= read -r line; do
  [ -n "$line" ] || continue
  name=$(sed -n 's/^{"name":"\([^"]*\)".*/\1/p' <<<"$line")
  version=$(sed -n 's/.*,"version":"\([^"]*\)".*/\1/p' <<<"$line")
  license=$(sed -n 's/.*,"license":"\([^"]*\)".*/\1/p' <<<"$line")
  authors=$(sed -n 's/.*,"authors":\[\([^]]*\)\].*/\1/p' <<<"$line")
  repo=$(sed -n 's/.*,"repository":"\([^"]*\)".*/\1/p' <<<"$line")

  # 自クレートは配布物の第三者ライセンスではないため除外する
  [ "$name" = "dskr" ] && continue

  if [ -n "$authors" ]; then
    authors=${authors//\"/}
    authors=${authors//</\&lt;}
    authors=${authors//>/\&gt;}
    authors=${authors//,/;}
  else
    authors="-"
  fi
  [ -n "$repo" ] || repo="-"
  [ -n "$license" ] || license="(宣言なし)"

  rows+="|$name|$version|$license|$authors|$repo|"$'\n'
  used_licenses+=" $license"
  count=$((count + 1))
done <<<"$packages"

sorted_rows=$(printf '%s' "$rows" | LC_ALL=C sort -t'|' -k1,1 -k2,2)

emit_license() {
  local title="$1" file="$2"
  echo "### $title"
  echo ""
  cat "third_party/licenses/$file"
  echo ""
}

{
  cat <<'HEAD'
# 第三者ライセンス一覧

dskr は MIT ライセンスで公開しています (同梱の LICENSE を参照)。
このファイルは、dskr が依存するクレートのライセンスと著作権表示の一覧です。
クレートごとの完全なソースとライセンス文は https://crates.io/crates/<クレート名> を参照してください。

HEAD
  echo "## ライセンス本文"
  echo ""

  contains() { case "$used_licenses" in *"$1"*) return 0 ;; *) return 1 ;; esac; }
  if contains 'MIT'; then emit_license 'MIT License' 'MIT.txt'; fi
  if contains 'Apache-2.0'; then
    emit_license 'Apache License, Version 2.0' 'Apache-2.0.txt'
  fi
  if contains 'Zlib'; then emit_license 'zlib License' 'Zlib.txt'; fi
  if contains 'BSL-1.0'; then
    emit_license 'Boost Software License, Version 1.0' 'BSL-1.0.txt'
  fi
  if contains 'Unicode-3.0'; then emit_license 'Unicode License v3' 'Unicode-3.0.txt'; fi

  echo "## 依存クレート ($count 件)"
  echo ""
  echo "| クレート | バージョン | ライセンス | 著作権者 | リポジトリ |"
  echo "|---|---|---|---|---|"
  printf '%s' "$sorted_rows"
  echo ""
} > "$OUT"

echo "生成しました: $OUT ($count 件)"
