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
CANCEL_AFTER_MS=${BENCH_CANCEL_AFTER_MS:-0}
TOOLS=${BENCH_TOOLS:-""}
GDU_CMD=${BENCH_GDU_CMD:-"gdu -n"}
DUA_CMD=${BENCH_DUA_CMD:-"dua aggregate"}
NCDU_CMD=${BENCH_NCDU_CMD:-"ncdu -o"}

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
if ! [[ "$CANCEL_AFTER_MS" =~ ^[0-9]+$ ]]; then
    echo "BENCH_CANCEL_AFTER_MS が不正です: $CANCEL_AFTER_MS" >&2
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

mkdir -p "$BASE"

tool_bin() {
    local cmd=$1
    printf '%s\n' "${cmd%% *}"
}

tool_version() {
    local cmd=$1
    local bin
    bin=$(tool_bin "$cmd")
    "$bin" --version 2>/dev/null | head -1 || true
}

print_metadata() {
    local cpu fs cache mem storage threads empty
    cpu="unknown"
    if [ -r /proc/cpuinfo ]; then
        cpu=$(awk -F: '/model name/{gsub(/^[ \t]+/, "", $2); print $2; exit}' /proc/cpuinfo)
    elif command -v sysctl >/dev/null 2>&1; then
        cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)
    fi
    fs=$(df -h "$BASE" 2>/dev/null | tail -1 | tr -s ' ' || true)
    mem="unknown"
    if [ -r /proc/meminfo ]; then
        mem=$(awk '/^MemTotal:/{printf "%.1fGiB", $2/1024/1024}' /proc/meminfo)
    elif command -v sysctl >/dev/null 2>&1; then
        local bytes
        bytes=$(sysctl -n hw.memsize 2>/dev/null || true)
        if [[ "$bytes" =~ ^[0-9]+$ ]]; then
            mem=$(awk -v b="$bytes" 'BEGIN { printf "%.1fGiB", b/1024/1024/1024 }')
        fi
    fi
    storage="unknown"
    if command -v lsblk >/dev/null 2>&1; then
        local dev
        dev=$(df -P "$BASE" 2>/dev/null | awk 'NR == 2 { print $1 }')
        if [ -n "$dev" ]; then
            storage=$(lsblk -dno MODEL "$dev" 2>/dev/null | head -1 | tr -s ' ' || true)
        fi
    fi
    threads=$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo unknown)
    if [ "$FILE_SIZE" -eq 0 ]; then
        empty=100
    else
        empty=0
    fi
    if [ "$DROP_CACHES" = "1" ]; then
        cache="cold-attempt"
    else
        cache="warm"
    fi

    printf '# generated_at=%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf '# os=%s\n' "$(uname -srm 2>/dev/null || echo unknown)"
    printf '# cpu=%s\n' "${cpu:-unknown}"
    printf '# cpu_threads=%s\n' "${threads:-unknown}"
    printf '# memory=%s\n' "${mem:-unknown}"
    printf '# storage=%s\n' "${storage:-unknown}"
    printf '# fs=%s\n' "${fs:-unknown}"
    printf '# cache=%s\n' "$cache"
    printf '# layout=flat-dirs depth=2 files_per_dir=%s empty_ratio=%s%% hardlinks=0\n' \
        "$FILES_PER_DIR" "$empty"
    printf '# base=%s\n' "$BASE"
    printf '# sizes=%s\n' "$SIZES"
    printf '# files_per_dir=%s file_size=%s repeats=%s warmup=%s\n' \
        "$FILES_PER_DIR" "$FILE_SIZE" "$REPEATS" "$WARMUP"
    printf '# dskr_format=json\n'
    printf '# dskr=%s\n' "$("$BIN" --version 2>/dev/null | head -1 || true)"
    printf '# commit=%s\n' "$(git -C "$ROOT_DIR" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    for tool in $TOOLS; do
        case "$tool" in
            gdu) cmd=$GDU_CMD ;;
            dua) cmd=$DUA_CMD ;;
            ncdu) cmd=$NCDU_CMD ;;
            *) continue ;;
        esac
        bin=$(tool_bin "$cmd")
        if command -v "$bin" >/dev/null 2>&1; then
            printf '# tool=%s version=%s\n' "$tool" "$(tool_version "$cmd")"
        fi
    done
}

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
            if [ "$FILE_SIZE" -eq 0 ]; then
                : >"$root/d$d/f$i"
            else
                head -c "$FILE_SIZE" /dev/zero >"$root/d$d/f$i"
            fi
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

# SIGINT を送ってから終了するまでの時間を wall_ms として返す。
measure_cancel() {
    local label=$1
    local delay_ms=$2
    shift 2

    local err="$TMP_OUT/err"
    local start kill_at end pid rss=0 status=0
    local delay_s
    delay_s=$(awk -v ms="$delay_ms" 'BEGIN { printf "%.3f", ms / 1000 }')
    start=$(now_ns)
    ( trap - INT; exec "$@" ) >/dev/null 2>"$err" &
    pid=$!
    sleep "$delay_s"
    rss=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d '[:space:]' || true)
    [[ "$rss" =~ ^[0-9]+$ ]] || rss=0
    kill_at=$(now_ns)
    kill -INT "$pid" 2>/dev/null || true
    if wait "$pid"; then
        status=0
    else
        status=$?
    fi
    end=$(now_ns)
    local cancel_ms=$(( (end - kill_at) / 1000000 ))
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$label" "$cancel_ms" "$rss" "0" "0" "0" "$status"
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

run_tool() {
    local tool=$1
    local count=$2
    local root=$3
    local cmd
    case "$tool" in
        gdu) cmd=$GDU_CMD ;;
        dua) cmd=$DUA_CMD ;;
        ncdu) cmd=$NCDU_CMD ;;
        *) return 0 ;;
    esac

    local bin
    bin=$(tool_bin "$cmd")
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "# skip tool=$tool count=$count reason=not-installed" >&2
        return 0
    fi

    local -a args
    read -r -a args <<<"$cmd"
    local r
    for ((r = 1; r <= REPEATS; r++)); do
        if [ "$tool" = "ncdu" ]; then
            measure "$tool-$count-$r" "${args[@]}" \
                "$TMP_OUT/ncdu-$count-$r.export" "$root"
        else
            measure "$tool-$count-$r" "${args[@]}" "$root"
        fi
    done
}

print_metadata

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

    for tool in $TOOLS; do
        run_tool "$tool" "$count" "$root"
    done

    if [ "$CANCEL_AFTER_MS" -gt 0 ]; then
        measure_cancel "dskr-$count-cancel" "$CANCEL_AFTER_MS" \
            "$BIN" "$root" --no-tui --format json
    fi
done

if [ "$KEEP" != "1" ]; then
    rm -rf "$BASE"
else
    echo "フィクスチャ: $BASE (BENCH_KEEP=0 で終了時に削除)" >&2
fi
