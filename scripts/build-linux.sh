#!/usr/bin/env bash
# dskr の Linux 向けリリースビルド。
#
# 既定では x86_64-unknown-linux-musl をビルドし、tar.gz を作成します。
# --deb / --rpm / --all を指定すると、同じ静的バイナリから .deb / .rpm も
# 作成します。配布用のダウンロードパッケージが目的で、ディストリビューション
# の公式リポジトリ向けではありません。
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
OUT_DIR="$ROOT_DIR/dist"
TARGET=${TARGET:-x86_64-unknown-linux-musl}
DO_TAR=0
DO_DEB=0
DO_RPM=0
DO_TEST=0
DO_CLEAN=0
DO_SMOKE=1

usage() {
    cat <<'EOF'
使い方: scripts/build-linux.sh [オプション]

オプション:
  --tar             tar.gz と .sha256 を作成 (指定がない場合の既定)
  --deb             .deb と .sha256 を作成
  --rpm             .rpm と .sha256 を作成
  --all             tar.gz / .deb / .rpm をすべて作成
  --test            ビルド前にホストで cargo test --release を実行
  --clean           ビルド前に cargo clean
  --no-smoke        生成バイナリの起動確認を省略
  --out-dir DIR     出力先 (既定: dist)
  --target TRIPLE   ターゲット (既定: x86_64-unknown-linux-musl)
  -h, --help        このヘルプ

必要なツール:
  rustup target add x86_64-unknown-linux-musl
  sudo apt-get install -y musl-tools rpm cpio   # .rpm を作る場合
EOF
}

fail() {
    echo "エラー: $*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "$1 が見つかりません。$2"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --tar)
            DO_TAR=1
            ;;
        --deb)
            DO_DEB=1
            ;;
        --rpm)
            DO_RPM=1
            ;;
        --all)
            DO_TAR=1
            DO_DEB=1
            DO_RPM=1
            ;;
        --test)
            DO_TEST=1
            ;;
        --clean)
            DO_CLEAN=1
            ;;
        --no-smoke)
            DO_SMOKE=0
            ;;
        --out-dir)
            shift
            [ $# -gt 0 ] || fail "--out-dir に値を指定してください"
            OUT_DIR=$1
            ;;
        --target)
            shift
            [ $# -gt 0 ] || fail "--target に値を指定してください"
            TARGET=$1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "不明なオプションです: $1 (--help を参照)"
            ;;
    esac
    shift
done

if [ "$DO_TAR" -eq 0 ] && [ "$DO_DEB" -eq 0 ] && [ "$DO_RPM" -eq 0 ]; then
    DO_TAR=1
fi

cd "$ROOT_DIR"
[ -f Cargo.toml ] || fail "Cargo.toml が見つかりません: $ROOT_DIR"
[ -f Cargo.lock ] || fail "Cargo.lock が見つかりません"

VERSION=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)
[ -n "$VERSION" ] || fail "Cargo.toml からバージョンを取得できません"

case "$TARGET" in
    x86_64-*)
        ARCH=x86_64
        DEB_ARCH=amd64
        RPM_ARCH=x86_64
        ;;
    aarch64-*)
        ARCH=aarch64
        DEB_ARCH=arm64
        RPM_ARCH=aarch64
        ;;
    *)
        fail "未対応のターゲットです: $TARGET"
        ;;
esac

PKG_NAME="dskr-$VERSION-linux-$ARCH"
BIN="$ROOT_DIR/target/$TARGET/release/dskr"
STAGE_ROOT="$OUT_DIR/.stage-$PKG_NAME"
STAGE="$STAGE_ROOT/$PKG_NAME"
DEB_ROOT="$OUT_DIR/.deb-$PKG_NAME"
RPM_TOP="$OUT_DIR/.rpmbuild-$PKG_NAME"

DOCS=(
    README.md
    USAGE.md
    ACCURACY.md
    LICENSE
    LICENSE-MUSL
    LICENSE-RUST-MIT
    LICENSE-RUST-APACHE
    THIRD-PARTY-LICENSES.md
)

cleanup() {
    rm -rf "$STAGE_ROOT" "$DEB_ROOT" "$RPM_TOP"
}
trap cleanup EXIT

need_cmd cargo "Rust を導入してください"
need_cmd rustup "rustup を導入してください"
need_cmd tar "tar を導入してください"
need_cmd sha256sum "coreutils を導入してください"

if ! rustup target list --installed | grep -qx "$TARGET"; then
    fail "ターゲット $TARGET が未導入です。次を実行してください:
  rustup target add $TARGET"
fi

if [ "$DO_DEB" -eq 1 ]; then
    need_cmd dpkg-deb "dpkg-dev を導入してください"
fi

if [ "$DO_RPM" -eq 1 ]; then
    need_cmd rpmbuild "rpm を導入してください (例: sudo apt-get install -y rpm)"
fi

for doc in "${DOCS[@]}"; do
    [ -f "$ROOT_DIR/$doc" ] || fail "同梱するファイルが見つかりません: $doc"
done
[ -f "$ROOT_DIR/packaging/debian/copyright" ] || fail "packaging/debian/copyright が見つかりません"

mkdir -p "$OUT_DIR"
rm -rf "$STAGE_ROOT" "$DEB_ROOT" "$RPM_TOP"

if [ "$DO_CLEAN" -eq 1 ]; then
    echo "==> cargo clean"
    cargo clean
fi

if [ "$DO_TEST" -eq 1 ]; then
    echo "==> cargo test --release --locked"
    cargo test --release --locked
fi

# 配布バイナリにビルド環境のローカルパスを残さない。
REMAP="--remap-path-prefix=$HOME=/build --remap-path-prefix=$ROOT_DIR=/src"
if [ -n "${RUSTFLAGS:-}" ]; then
    export RUSTFLAGS="$RUSTFLAGS $REMAP"
else
    export RUSTFLAGS="$REMAP"
fi
echo "==> RUSTFLAGS: $RUSTFLAGS"
echo "==> cargo build --release --locked --target $TARGET"
cargo build --release --locked --target "$TARGET"

[ -x "$BIN" ] || fail "生成物が見つかりません: $BIN"

if [ "$DO_SMOKE" -eq 1 ]; then
    echo "==> 起動確認"
    "$BIN" --version
    "$BIN" "$ROOT_DIR" --no-tui --max-depth 1 --top 5 >/dev/null
fi

if [[ "$TARGET" == *musl* ]]; then
    if command -v ldd >/dev/null 2>&1; then
        if ! ldd "$BIN" 2>&1 | grep -qiE 'statically linked|not a dynamic executable'; then
            fail "静的リンクになっていません: $BIN"
        fi
        echo "==> 静的リンクを確認"
    fi
fi

# tar.gz 用のステージ
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/dskr"
chmod 0755 "$STAGE/dskr"
for doc in "${DOCS[@]}"; do
    cp "$ROOT_DIR/$doc" "$STAGE/$doc"
done
(
    cd "$STAGE"
    sha256sum dskr > SHA256SUMS.txt
)

write_sha256() {
    local file=$1
    (
        cd "$(dirname "$file")"
        sha256sum "$(basename "$file")" > "$(basename "$file").sha256"
    )
}

if [ "$DO_TAR" -eq 1 ]; then
    echo "==> tar.gz を作成"
    TARBALL="$OUT_DIR/$PKG_NAME.tar.gz"
    rm -f "$TARBALL" "$TARBALL.sha256"
    tar -C "$STAGE_ROOT" -czf "$TARBALL" "$PKG_NAME"
    write_sha256 "$TARBALL"
fi

if [ "$DO_DEB" -eq 1 ]; then
    echo "==> .deb を作成"
    mkdir -p "$DEB_ROOT/DEBIAN" "$DEB_ROOT/usr/bin" "$DEB_ROOT/usr/share/doc/dskr"
    cp "$BIN" "$DEB_ROOT/usr/bin/dskr"
    chmod 0755 "$DEB_ROOT/usr/bin/dskr"
    for doc in "${DOCS[@]}"; do
        cp "$ROOT_DIR/$doc" "$DEB_ROOT/usr/share/doc/dskr/$doc"
    done
    cp "$ROOT_DIR/packaging/debian/copyright" "$DEB_ROOT/usr/share/doc/dskr/copyright"
    INSTALLED_SIZE=$(du -sk "$DEB_ROOT/usr" | awk '{print $1}')
    cat > "$DEB_ROOT/DEBIAN/control" <<EOF
Package: dskr
Version: $VERSION-1
Section: utils
Priority: optional
Architecture: $DEB_ARCH
Installed-Size: $INSTALLED_SIZE
Maintainer: QQK <QuunQuickKick@users.noreply.github.com>
Homepage: https://github.com/QuunQuickKick/dskr
Description: fast file-size and disk-usage finder with a TUI
 dskr scans directories and reports allocated and logical sizes.
 It provides a terminal UI and headless report/tree/flat/JSON/CSV output.
EOF
    chmod 0644 "$DEB_ROOT/DEBIAN/control"
    DEB_FILE="$OUT_DIR/dskr_${VERSION}-1_${DEB_ARCH}.deb"
    rm -f "$DEB_FILE" "$DEB_FILE.sha256"
    dpkg-deb --build --root-owner-group "$DEB_ROOT" "$DEB_FILE"
    write_sha256 "$DEB_FILE"
fi

if [ "$DO_RPM" -eq 1 ]; then
    echo "==> .rpm を作成"
    mkdir -p "$RPM_TOP"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
    cp "$BIN" "$RPM_TOP/SOURCES/dskr"
    SPEC="$RPM_TOP/SPECS/dskr.spec"
    CHANGELOG_DATE=$(LC_ALL=C date -u '+%a %b %d %Y')
    cat > "$SPEC" <<EOF
Name: dskr
Version: $VERSION
Release: 1
Summary: Fast file-size and disk-usage finder with a TUI
License: MIT
URL: https://github.com/QuunQuickKick/dskr
BuildArch: $RPM_ARCH
AutoReqProv: no
Source0: dskr

%description
dskr scans directories and reports allocated and logical sizes.
It provides a terminal UI and headless report/tree/flat/JSON/CSV output.

%install
rm -rf %{buildroot}
install -Dm755 %{_sourcedir}/dskr %{buildroot}/usr/bin/dskr
install -Dm644 %{_sourcedir}/README.md %{buildroot}/usr/share/doc/dskr/README.md
install -Dm644 %{_sourcedir}/USAGE.md %{buildroot}/usr/share/doc/dskr/USAGE.md
install -Dm644 %{_sourcedir}/ACCURACY.md %{buildroot}/usr/share/doc/dskr/ACCURACY.md
install -Dm644 %{_sourcedir}/LICENSE %{buildroot}/usr/share/licenses/dskr/LICENSE
install -Dm644 %{_sourcedir}/LICENSE-MUSL %{buildroot}/usr/share/licenses/dskr/LICENSE-MUSL
install -Dm644 %{_sourcedir}/LICENSE-RUST-MIT %{buildroot}/usr/share/licenses/dskr/LICENSE-RUST-MIT
install -Dm644 %{_sourcedir}/LICENSE-RUST-APACHE %{buildroot}/usr/share/licenses/dskr/LICENSE-RUST-APACHE
install -Dm644 %{_sourcedir}/THIRD-PARTY-LICENSES.md %{buildroot}/usr/share/licenses/dskr/THIRD-PARTY-LICENSES.md

%files
%attr(0755,root,root) /usr/bin/dskr
%dir /usr/share/doc/dskr
/usr/share/doc/dskr/README.md
/usr/share/doc/dskr/USAGE.md
/usr/share/doc/dskr/ACCURACY.md
%dir /usr/share/licenses/dskr
/usr/share/licenses/dskr/LICENSE
/usr/share/licenses/dskr/LICENSE-MUSL
/usr/share/licenses/dskr/LICENSE-RUST-MIT
/usr/share/licenses/dskr/LICENSE-RUST-APACHE
/usr/share/licenses/dskr/THIRD-PARTY-LICENSES.md

%changelog
* $CHANGELOG_DATE QQK <QuunQuickKick@users.noreply.github.com> - $VERSION-1
- Release $VERSION
EOF
    # 同梱ドキュメントを SOURCES に置く
    for doc in "${DOCS[@]}"; do
        cp "$ROOT_DIR/$doc" "$RPM_TOP/SOURCES/$doc"
    done
    rpmbuild -bb \
        --define "_topdir $RPM_TOP" \
        --define "_sourcedir $RPM_TOP/SOURCES" \
        --define "_builddir $RPM_TOP/BUILD" \
        --define "_buildrootdir $RPM_TOP/BUILDROOT" \
        --define "_rpmdir $RPM_TOP/RPMS" \
        --define "_specdir $RPM_TOP/SPECS" \
        --define "_srcrpmdir $RPM_TOP/SRPMS" \
        "$SPEC"
    RPM_FILE=$(find "$RPM_TOP/RPMS" -type f -name '*.rpm' -print -quit)
    [ -n "$RPM_FILE" ] || fail "rpmbuild の出力が見つかりません"
    RPM_OUT="$OUT_DIR/$(basename "$RPM_FILE")"
    cp "$RPM_FILE" "$RPM_OUT"
    write_sha256 "$RPM_OUT"
fi

echo
echo "==> 完了"
echo "  バージョン: $VERSION"
echo "  出力先    : $OUT_DIR"
for file in "$OUT_DIR"/*; do
    case "$file" in
        *.tar.gz|*.deb|*.rpm)
            echo "  - $(basename "$file")"
            ;;
    esac
done
