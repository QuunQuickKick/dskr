#!/usr/bin/env bash
# Linux 向けリリース成果物 (tar.gz / .deb / .rpm) を検証する。
# CI とリリースワークフローの両方で使う。
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
DIST=${1:-"$ROOT_DIR/dist"}

fail() {
    echo "エラー: $*" >&2
    exit 1
}

check_file() {
    [ -e "$1" ] || fail "ファイルがありません: $1"
}

shopt -s nullglob
TARBALLS=("$DIST"/*.tar.gz)
DEBS=("$DIST"/*.deb)
RPMS=("$DIST"/*.rpm)
shopt -u nullglob

[ "${#TARBALLS[@]}" -eq 1 ] || fail "tar.gz は 1 個である必要があります: ${TARBALLS[*]:-なし}"
[ "${#DEBS[@]}" -eq 1 ] || fail ".deb は 1 個である必要があります: ${DEBS[*]:-なし}"
[ "${#RPMS[@]}" -eq 1 ] || fail ".rpm は 1 個である必要があります: ${RPMS[*]:-なし}"

TARBALL=${TARBALLS[0]}
DEB=${DEBS[0]}
RPM=${RPMS[0]}

for pkg in "$TARBALL" "$DEB" "$RPM"; do
    check_file "$pkg.sha256"
    (
        cd "$DIST"
        sha256sum -c "$(basename "$pkg").sha256"
    )
done

TMP=$(mktemp -d "${TMPDIR:-/tmp}/dskr-pkg-check.XXXXXX")
cleanup() {
    rm -rf "$TMP"
}
trap cleanup EXIT

DOCS=(README.md USAGE.md ACCURACY.md LICENSE LICENSE-MUSL LICENSE-RUST-MIT LICENSE-RUST-APACHE THIRD-PARTY-LICENSES.md)

# tar.gz
tar -xzf "$TARBALL" -C "$TMP"
TOP=$(find "$TMP" -mindepth 1 -maxdepth 1 -type d -print -quit)
[ -n "$TOP" ] || fail "tar.gz の展開先が見つかりません"
check_file "$TOP/dskr"
[ -x "$TOP/dskr" ] || fail "tar.gz の dskr に実行権限がありません"
for doc in "${DOCS[@]}"; do
    check_file "$TOP/$doc"
done
check_file "$TOP/SHA256SUMS.txt"
(
    cd "$TOP"
    sha256sum -c SHA256SUMS.txt
)
"$TOP/dskr" --version >/dev/null
if command -v ldd >/dev/null 2>&1; then
    ldd "$TOP/dskr" 2>&1 | grep -qiE 'statically linked|not a dynamic executable' \
        || fail "tar.gz の dskr が静的リンクではありません"
fi

# .deb
check_file "$DEB"
dpkg-deb --info "$DEB" >/dev/null
dpkg-deb --contents "$DEB" >/dev/null
DEB_ROOT="$TMP/deb"
mkdir -p "$DEB_ROOT"
dpkg-deb -x "$DEB" "$DEB_ROOT"
check_file "$DEB_ROOT/usr/bin/dskr"
[ -x "$DEB_ROOT/usr/bin/dskr" ] || fail ".deb の dskr に実行権限がありません"
for doc in "${DOCS[@]}"; do
    check_file "$DEB_ROOT/usr/share/doc/dskr/$doc"
done
check_file "$DEB_ROOT/usr/share/doc/dskr/copyright"
"$DEB_ROOT/usr/bin/dskr" --version >/dev/null
if command -v ldd >/dev/null 2>&1; then
    ldd "$DEB_ROOT/usr/bin/dskr" 2>&1 | grep -qiE 'statically linked|not a dynamic executable' \
        || fail ".deb の dskr が静的リンクではありません"
fi

# .rpm
check_file "$RPM"
rpm -qip "$RPM" >/dev/null 2>&1
rpm -qlp "$RPM" 2>/dev/null | grep -qx '/usr/bin/dskr' || fail ".rpm に /usr/bin/dskr がありません"
for doc in README.md USAGE.md ACCURACY.md; do
    rpm -qlp "$RPM" 2>/dev/null | grep -qx "/usr/share/doc/dskr/$doc" \
        || fail ".rpm に /usr/share/doc/dskr/$doc がありません"
done
for doc in LICENSE LICENSE-MUSL LICENSE-RUST-MIT LICENSE-RUST-APACHE THIRD-PARTY-LICENSES.md; do
    rpm -qlp "$RPM" 2>/dev/null | grep -qx "/usr/share/licenses/dskr/$doc" \
        || fail ".rpm に /usr/share/licenses/dskr/$doc がありません"
done
RPM_ROOT="$TMP/rpm"
mkdir -p "$RPM_ROOT"
(
    cd "$RPM_ROOT"
    rpm2cpio "$RPM" | cpio -idm --quiet
)
check_file "$RPM_ROOT/usr/bin/dskr"
[ -x "$RPM_ROOT/usr/bin/dskr" ] || fail ".rpm の dskr に実行権限がありません"
"$RPM_ROOT/usr/bin/dskr" --version >/dev/null
if command -v ldd >/dev/null 2>&1; then
    ldd "$RPM_ROOT/usr/bin/dskr" 2>&1 | grep -qiE 'statically linked|not a dynamic executable' \
        || fail ".rpm の dskr が静的リンクではありません"
fi

echo "==> Linux パッケージ検証 OK"
echo "  tar.gz: $(basename "$TARBALL")"
echo "  .deb  : $(basename "$DEB")"
echo "  .rpm  : $(basename "$RPM")"
