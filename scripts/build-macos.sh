#!/usr/bin/env bash
# dskr の macOS 向けリリースビルド。
#
# 既定では aarch64-apple-darwin と x86_64-apple-darwin をビルドし、
# lipo で universal2 の tar.gz を作成します。Apple Silicon / Intel の
# 両方で動作します。配布用のダウンロードパッケージが目的です。
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
OUT_DIR="$ROOT_DIR/dist"
ARCH_MODE=universal
DO_TEST=0
DO_CLEAN=0
DO_SMOKE=1

usage() {
    cat <<'EOF'
使い方: scripts/build-macos.sh [オプション]

オプション:
  --universal       aarch64 + x86_64 の universal2 を作成 (既定)
  --arch ARCH       aarch64 または x86_64 の単一アーキテクチャ
  --tar             tar.gz と .sha256 を作成 (既定)
  --test            ビルド前に cargo test --release を実行
  --clean           ビルド前に cargo clean
  --no-smoke        生成バイナリの起動確認を省略
  --out-dir DIR     出力先 (既定: dist)
  -h, --help        このヘルプ

必要なもの:
  macOS + Xcode Command Line Tools
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
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
        --universal)
            ARCH_MODE=universal
            ;;
        --arch)
            shift
            [ $# -gt 0 ] || fail "--arch に値を指定してください"
            case "$1" in
                aarch64|x86_64)
                    ARCH_MODE=$1
                    ;;
                *)
                    fail "--arch は aarch64 または x86_64 を指定してください: $1"
                    ;;
            esac
            ;;
        --tar)
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

[ "$(uname -s)" = "Darwin" ] || fail "macOS で実行してください"

cd "$ROOT_DIR"
[ -f Cargo.toml ] || fail "Cargo.toml が見つかりません: $ROOT_DIR"
[ -f Cargo.lock ] || fail "Cargo.lock が見つかりません"

VERSION=$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)
[ -n "$VERSION" ] || fail "Cargo.toml からバージョンを取得できません"

case "$ARCH_MODE" in
    universal)
        TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
        ARCH_LABEL=universal2
        ;;
    aarch64)
        TARGETS=(aarch64-apple-darwin)
        ARCH_LABEL=aarch64
        ;;
    x86_64)
        TARGETS=(x86_64-apple-darwin)
        ARCH_LABEL=x86_64
        ;;
    *)
        fail "内部エラー: 不正な ARCH_MODE です: $ARCH_MODE"
        ;;
esac

PKG_NAME="dskr-$VERSION-macos-$ARCH_LABEL"
BIN_DIR="$ROOT_DIR/target/macos-$ARCH_LABEL"
BIN="$BIN_DIR/dskr"
STAGE_ROOT="$OUT_DIR/.stage-$PKG_NAME"
STAGE="$STAGE_ROOT/$PKG_NAME"

DOCS=(
    README.md
    USAGE.md
    ACCURACY.md
    LICENSE
    LICENSE-RUST-MIT
    LICENSE-RUST-APACHE
    THIRD-PARTY-LICENSES.md
)

cleanup() {
    rm -rf "$STAGE_ROOT"
}
trap cleanup EXIT

need_cmd cargo "Rust を導入してください"
need_cmd rustup "rustup を導入してください"
need_cmd lipo "Xcode Command Line Tools を導入してください"
need_cmd tar "tar を導入してください"
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    fail "sha256sum または shasum が必要です"
fi

for target in "${TARGETS[@]}"; do
    if ! rustup target list --installed | grep -qx "$target"; then
        fail "ターゲット $target が未導入です。次を実行してください:
  rustup target add ${TARGETS[*]}"
    fi
done

for doc in "${DOCS[@]}"; do
    [ -f "$ROOT_DIR/$doc" ] || fail "同梱するファイルが見つかりません: $doc"
done

mkdir -p "$OUT_DIR" "$BIN_DIR"
rm -rf "$STAGE_ROOT" "$BIN_DIR"

if [ "$DO_CLEAN" -eq 1 ]; then
    echo "==> cargo clean"
    cargo clean
fi

if [ "$DO_TEST" -eq 1 ]; then
    echo "==> cargo test --release --locked"
    cargo test --release --locked
fi

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
REMAP="--remap-path-prefix=$HOME=/build --remap-path-prefix=$ROOT_DIR=/src"
if [ -n "${RUSTFLAGS:-}" ]; then
    export RUSTFLAGS="$RUSTFLAGS $REMAP"
else
    export RUSTFLAGS="$REMAP"
fi
echo "==> MACOSX_DEPLOYMENT_TARGET: $MACOSX_DEPLOYMENT_TARGET"
echo "==> RUSTFLAGS: $RUSTFLAGS"

BINARIES=()
for target in "${TARGETS[@]}"; do
    echo "==> cargo build --release --locked --target $target"
    cargo build --release --locked --target "$target"
    target_bin="$ROOT_DIR/target/$target/release/dskr"
    [ -x "$target_bin" ] || fail "生成物が見つかりません: $target_bin"
    BINARIES+=("$target_bin")
done

if [ "$ARCH_MODE" = "universal" ]; then
    echo "==> universal2 を作成"
    lipo -create "${BINARIES[@]}" -output "$BIN"
else
    cp "${BINARIES[0]}" "$BIN"
fi
chmod 0755 "$BIN"

# lipo 後の universal バイナリは ad-hoc 署名し直す。
# これは Gatekeeper の notarization ではなく、Apple Silicon で実行可能にするため。
if command -v codesign >/dev/null 2>&1; then
    codesign --force --sign - "$BIN" >/dev/null
fi

if [ "$ARCH_MODE" = "universal" ]; then
    ARCHS=$(lipo -archs "$BIN")
    [[ "$ARCHS" == *arm64* ]] || fail "universal2 に arm64 がありません: $ARCHS"
    [[ "$ARCHS" == *x86_64* ]] || fail "universal2 に x86_64 がありません: $ARCHS"
    echo "==> lipo: $ARCHS"
fi

if [ "$DO_SMOKE" -eq 1 ]; then
    echo "==> 起動確認"
    "$BIN" --version
    "$BIN" "$ROOT_DIR" --no-tui --max-depth 1 --top 5 >/dev/null
fi

sha256_hex() {
    local file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    else
        shasum -a 256 "$file" | awk '{print $1}'
    fi
}

write_sha256() {
    local file=$1
    printf '%s  %s\n' "$(sha256_hex "$file")" "$(basename "$file")" > "$file.sha256"
}

echo "==> tar.gz を作成"
mkdir -p "$STAGE"
cp "$BIN" "$STAGE/dskr"
chmod 0755 "$STAGE/dskr"
for doc in "${DOCS[@]}"; do
    cp "$ROOT_DIR/$doc" "$STAGE/$doc"
done
printf '%s  dskr\n' "$(sha256_hex "$STAGE/dskr")" > "$STAGE/SHA256SUMS.txt"

TARBALL="$OUT_DIR/$PKG_NAME.tar.gz"
rm -f "$TARBALL" "$TARBALL.sha256"
# macOS の tar が AppleDouble (._*) を混ぜないようにする。
export COPYFILE_DISABLE=1
tar -C "$STAGE_ROOT" -czf "$TARBALL" "$PKG_NAME"
write_sha256 "$TARBALL"

echo
echo "==> 完了"
echo "  バージョン: $VERSION"
echo "  出力先    : $OUT_DIR"
echo "  - $(basename "$TARBALL")"
echo "  - $(basename "$TARBALL").sha256"
