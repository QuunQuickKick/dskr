#!/usr/bin/env bash
# macOS 向けリリース成果物 (universal2 tar.gz) を検証する。
# CI とリリースワークフローの両方で使う。
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
DIST=${1:-"$ROOT_DIR/dist"}
DIST=$(cd "$DIST" && pwd) || {
    echo "エラー: 出力先が見つかりません: $DIST" >&2
    exit 1
}

fail() {
    echo "エラー: $*" >&2
    exit 1
}

check_file() {
    [ -e "$1" ] || fail "ファイルがありません: $1"
}

verify_sha() {
    local file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        (
            cd "$(dirname "$file")"
            sha256sum -c "$(basename "$file").sha256"
        )
    else
        (
            cd "$(dirname "$file")"
            shasum -a 256 -c "$(basename "$file").sha256"
        )
    fi
}

shopt -s nullglob
TARBALLS=("$DIST"/*.tar.gz)
shopt -u nullglob
[ "${#TARBALLS[@]}" -eq 1 ] || fail "tar.gz は 1 個である必要があります: ${TARBALLS[*]:-なし}"
TARBALL=${TARBALLS[0]}

check_file "$TARBALL.sha256"
verify_sha "$TARBALL"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/dskr-macos-check.XXXXXX")
cleanup() {
    rm -rf "$TMP"
}
trap cleanup EXIT

TOP_NAME=$(tar -tzf "$TARBALL" > "$TMP/list" && awk -F/ 'NR == 1 { print $1; exit }' "$TMP/list")
[ -n "$TOP_NAME" ] || fail "tar.gz の先頭ディレクトリを取得できません"
tar -xzf "$TARBALL" -C "$TMP"
TOP="$TMP/$TOP_NAME"
[ -d "$TOP" ] || fail "tar.gz の展開先が見つかりません: $TOP"

DOCS=(README.md USAGE.md ACCURACY.md LICENSE LICENSE-RUST-MIT LICENSE-RUST-APACHE THIRD-PARTY-LICENSES.md)
check_file "$TOP/dskr"
[ -x "$TOP/dskr" ] || fail "dskr に実行権限がありません"
for doc in "${DOCS[@]}"; do
    check_file "$TOP/$doc"
done
check_file "$TOP/SHA256SUMS.txt"
if command -v sha256sum >/dev/null 2>&1; then
    (
        cd "$TOP"
        sha256sum -c SHA256SUMS.txt
    )
else
    (
        cd "$TOP"
        shasum -a 256 -c SHA256SUMS.txt
    )
fi

ARCHS=$(lipo -archs "$TOP/dskr")
[[ "$ARCHS" == *arm64* ]] || fail "universal2 に arm64 がありません: $ARCHS"
[[ "$ARCHS" == *x86_64* ]] || fail "universal2 に x86_64 がありません: $ARCHS"
echo "==> lipo: $ARCHS"

if command -v codesign >/dev/null 2>&1; then
    codesign --verify "$TOP/dskr" || fail "ad-hoc 署名の検証に失敗しました"
fi

"$TOP/dskr" --version >/dev/null
"$TOP/dskr" "$ROOT_DIR" --no-tui --max-depth 1 --top 5 >/dev/null

echo "==> macOS パッケージ検証 OK"
echo "  tar.gz: $(basename "$TARBALL")"
echo "  arch  : $ARCHS"
