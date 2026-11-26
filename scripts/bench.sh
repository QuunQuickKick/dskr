#!/usr/bin/env bash
# dskr の簡易ベンチマーク。
#
# 指定した数のファイルを生成し、dskr の wall time / 最大 RSS / CPU / I/O を
# 複数回計測します。フィクスチャは既定で再利用します。
#
# 注意:
# - Linux では /proc から RSS と I/O を取得します。
# - macOS では ps から RSS と CPU を取得します (I/O は未対応)。
# - Windows は WSL など bash が動く環境で実行してください。
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
BIN=${DSKR_BIN:-"$ROOT_DIR/target/release/dskr"}
BASE=${BENCH_BASE:-"${TMPDIR:-/tmp}/dskr-bench"}
SIZES=${BENCH_SIZES:-"10000 100000"}
FILES_PER_DIR=${BENCH_FILES_PER_DIR:-100}
FILE_SIZE=${BENCH_FILE_SIZE:-1024}
REPEATS=${BENCH_REPEATS:-3}
WARMUP=${BENCH_WARMUP:-1}
KEEP=${BENCH_KEEP:-1}
DROP_CACHES=${BENCH_DROP_CACHES:-0}
WITH_DU=${BENCH_DU:-1}

TMP_OUT=$(mktemp -d "${TMPDIR:-/tmp}/dskr-bench-out.XXXXXX")
trap 'rm -rf "$TMP_OUT"' EXIT

if [ ! -x "$BIN" ]; then
    echo "dskr のバイナリが見つかりません: $BIN" >&2
    echo "先に cargo build --release を実行してください。" >&2
    exit 1
fi

for value in "$FILES_PER_DIR" "$REPEATS"; do
    if ! [[ "$value" =~ ^[0-9]+$ ]] || [ "$value" -le 0 ]; then
        echo "正の整数を指定してください: $value" >&2
        exit 1
    fi
done
if ! [[ "$WARMUP" =~ ^[0-9]+$ ]]; then
    echo "BENCH_WARMUP が不正です: $WARMUP" >&2
    exit 1
fi
if ! [[ "$FILE_SIZE" =~ ^[0-9]+$ ]]; then
    echo "BENCH_FILE_SIZE が不正です: $FILE_SIZE" >&2
    exit 1
fi

if ! command -v ps >/dev/null 2>&1; then
    echo "ps コマンドが必要です" >&2
    exit 1
fi

now_ns() {
    # GNU date はナノ秒まで取得できる。BSD date では秒精度にフォールバックする。
    local ns
    ns=$(date +%s%N 2>/dev/null || true)
    if [[ "$ns" =~ ^[0-9]+$ ]]; then
        printf '%s\n' "$ns"
    else
        printf '%s000000000\n' "$(date +%s)"
    fi
}

# ps の TIME ([[DD-]HH:]MM:SS) を秒に変換する。
cpu_seconds() {
    awk -F'[-:]' '
        NF == 3 { print $1 * 3600 + $2 * 60 + $3; next }
        NF == 2 { print $1 * 60 + $2; next }
        NF == 1 { print $1; next }
        { print 0 }
    ' <<<"$1"
}

drop_caches() {
    [ "$DROP_CACHES" = "1" ] || return 0
    if [ "$(id -u)" -ne 0 ]; then
        echo "警告: BENCH_DROP_CACHES=1 は root が必要です。スキップします" >&2
        return 0
    fi
    if [ -w /proc/sys/vm/drop_caches ]; then
        sync
        echo 3 > /proc/sys/vm/drop_caches
    else
        echo "警告: drop_caches を利用できません。スキップします" >&2
    fi
}

fixture() {
    local count=$1
    local root="$BASE/files-$count"
    local want="$count $FILES_PER_DIR $FILE_SIZE"
    if [ -f "$root/.fixture" ] && [ "$(cat "$root/.fixture")" = "$want" ]; then
        printf '%s\n' "$root"
        return
    fi

    echo "フィクスチャを生成: $root ($count files)" >&2
    rm -rf "$root"
    mkdir -p "$root"
    local dirs=$(( (count + FILES_PER_DIR - 1) / FILES_PER_DIR ))
    local n=0 d i
    for ((d = 0; d < dirs; d++)); do
        mkdir -p "$root/d$d"
        for ((i = 0; i < FILES_PER_DIR && n < count; i++, n++)); do
            head -c "$FILE_SIZE" /dev/zero >"$root/d$d/f$i"
        done
    done
    printf '%s\n' "$want" >"$root/.fixture"
    printf '%s\n' "$root"
}

# コマンドを実行し、wall_ms / max_rss_kib / cpu_s / read_bytes / write_bytes / status を返す。
measure() {
    local label=$1
    shift

    drop_caches
    local err="$TMP_OUT/err"
    local start end pid max_rss=0 rss cpu raw_read=0 raw_write=0 read_bytes=0 write_bytes=0 status=0
    local last_cpu="" last_ticks=""
    local clk_tck
    clk_tck=$(getconf CLK_TCK 2>/dev/null || echo 100)
    if ! [[ "$clk_tck" =~ ^[0-9]+$ ]] || [ "$clk_tck" -eq 0 ]; then
        clk_tck=100
    fi
    start=$(now_ns)
    "$@" >/dev/null 2>"$err" &
    pid=$!

    while kill -0 "$pid" 2>/dev/null; do
        rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d '[:space:]' || true)
        if [[ "$rss" =~ ^[0-9]+$ ]] && [ "$rss" -gt "$max_rss" ]; then
            max_rss=$rss
        fi
        cpu=$(ps -o time= -p "$pid" 2>/dev/null | tr -d '[:space:]' || true)
        if [ -n "$cpu" ]; then
            last_cpu=$cpu
        fi
        if [ -r "/proc/$pid/stat" ]; then
            last_ticks=$(awk '{print $14 + $15}' "/proc/$pid/stat" 2>/dev/null || true)
        fi
        if [ -r "/proc/$pid/io" ]; then
            raw_read=$(awk '/^read_bytes:/{print $2}' "/proc/$pid/io" 2>/dev/null || true)
            raw_write=$(awk '/^write_bytes:/{print $2}' "/proc/$pid/io" 2>/dev/null || true)
            [[ "$raw_read" =~ ^[0-9]+$ ]] && read_bytes=$raw_read
            [[ "$raw_write" =~ ^[0-9]+$ ]] && write_bytes=$raw_write
        fi
        sleep 0.01
    done

    if wait "$pid"; then
        status=0
    else
        status=$?
    fi
    end=$(now_ns)

    local wall_ms=$(( (end - start) / 1000000 ))
    local cpu_s
    if [[ "$last_ticks" =~ ^[0-9]+$ ]]; then
        cpu_s=$(awk -v ticks="$last_ticks" -v hz="$clk_tck" 'BEGIN { printf "%.3f", ticks / hz }')
    else
        cpu_s=$(cpu_seconds "${last_cpu:-}")
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$label" "$wall_ms" "$max_rss" "$cpu_s" "$read_bytes" "$write_bytes" "$status"
}

run_dskr() {
    local count=$1
    local root=$2
    local label=$3
    local r
    for ((r = 1; r <= REPEATS; r++)); do
        measure "$label-$r" "$BIN" "$root" --no-tui --format json
    done
}

COUNTS=()
ROOTS=()
for count in $SIZES; do
    if ! [[ "$count" =~ ^[0-9]+$ ]]; then
        echo "BENCH_SIZES の値が不正です: $count" >&2
        exit 1
    fi
    root=$(fixture "$count")
    COUNTS+=("$count")
    ROOTS+=("$root")
done

printf 'case\twall_ms\tmax_rss_kib\tcpu_s\tread_bytes\twrite_bytes\tstatus\n'

for i in "${!COUNTS[@]}"; do
    count=${COUNTS[$i]}
    root=${ROOTS[$i]}

    # ウォームアップ (ページキャッシュ・バイナリの読み込み)
    if [ "$WARMUP" -gt 0 ]; then
        measure "dskr-$count-warmup" "$BIN" "$root" --no-tui --format json >/dev/null
    fi
    run_dskr "$count" "$root" "dskr-$count"

    if [ "$WITH_DU" = "1" ] && command -v du >/dev/null 2>&1; then
        for ((r = 1; r <= REPEATS; r++)); do
            measure "du-$count-$r" du -s "$root"
        done
    fi
done

if [ "$KEEP" != "1" ]; then
    rm -rf "$BASE"
else
    echo "フィクスチャ: $BASE (BENCH_KEEP=0 で終了時に削除)" >&2
fi
