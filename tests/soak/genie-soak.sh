#!/bin/bash
# Orchestrate a genie-ai-runtime soak run on a Jetson (issue #113).
#
# Captures the environment and runtime systemd state, starts a tegrastats
# recorder, drives a paced chat/health workload via soak_driver.py for the
# requested duration, then scans dmesg/journal for OOM and scores the run with
# analyze_soak.py. Everything lands in a single timestamped run directory.
#
# Usage:
#   tests/soak/genie-soak.sh [--duration-h 24] [--interval 30] \
#       [--core-url http://127.0.0.1:3000] [--runtime-unit genie-ai-runtime.service] \
#       [--out-dir DIR] [--prompt TEXT] \
#       [--budget-p50-ms 1500] [--budget-p99-ms 4000] [--tegrastats-interval-ms 5000]
#
# Run it under tmux/nohup for a real 24h soak so an SSH drop does not kill it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

DURATION_H=24
INTERVAL=30
CORE_URL="http://127.0.0.1:3000"
RUNTIME_UNIT="genie-ai-runtime.service"
OUT_DIR=""
PROMPT=""
BUDGET_P50_MS=1500
BUDGET_P99_MS=4000
TEGRASTATS_INTERVAL_MS=5000

usage() {
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --duration-h) DURATION_H="$2"; shift 2 ;;
        --interval) INTERVAL="$2"; shift 2 ;;
        --core-url) CORE_URL="$2"; shift 2 ;;
        --runtime-unit) RUNTIME_UNIT="$2"; shift 2 ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --prompt) PROMPT="$2"; shift 2 ;;
        --budget-p50-ms) BUDGET_P50_MS="$2"; shift 2 ;;
        --budget-p99-ms) BUDGET_P99_MS="$2"; shift 2 ;;
        --tegrastats-interval-ms) TEGRASTATS_INTERVAL_MS="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

PY="$(command -v python3 || command -v python || true)"
if [ -z "$PY" ]; then
    echo "ERROR: python3 is required" >&2
    exit 1
fi

if [ -z "$OUT_DIR" ]; then
    OUT_DIR="$SCRIPT_DIR/runs/$(date -u +%Y%m%dT%H%M%SZ)"
fi
mkdir -p "$OUT_DIR"

TELEMETRY="$OUT_DIR/telemetry.jsonl"
TEGRASTATS_LOG="$OUT_DIR/tegrastats.log"
JOURNAL="$OUT_DIR/journal.txt"
META="$OUT_DIR/meta.json"
SUMMARY="$OUT_DIR/summary.json"
REPORT="$OUT_DIR/report.md"

DURATION_S="$(awk "BEGIN { printf \"%d\", $DURATION_H * 3600 }")"
START_ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
TEGRASTATS_PID=""

# systemd helpers degrade to empty/unknown off-systemd (dev hosts).
runtime_restarts() {
    if command -v systemctl > /dev/null 2>&1; then
        systemctl show "$RUNTIME_UNIT" -p NRestarts --value 2>/dev/null || echo ""
    else
        echo ""
    fi
}

cleanup() {
    if [ -n "$TEGRASTATS_PID" ] && kill -0 "$TEGRASTATS_PID" 2>/dev/null; then
        kill "$TEGRASTATS_PID" 2>/dev/null || true
        wait "$TEGRASTATS_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

L4T="unknown"
if [ -f /etc/nv_tegra_release ]; then
    L4T="$(head -n1 /etc/nv_tegra_release | tr -d '"')"
fi
RESTARTS_BEFORE="$(runtime_restarts)"

{
    echo "{"
    echo "  \"start_iso\": \"$START_ISO\","
    echo "  \"host\": \"$(hostname 2>/dev/null || echo unknown)\","
    echo "  \"l4t\": \"$L4T\","
    echo "  \"core_url\": \"$CORE_URL\","
    echo "  \"runtime_unit\": \"$RUNTIME_UNIT\","
    echo "  \"duration_h\": $DURATION_H,"
    echo "  \"interval_s\": $INTERVAL,"
    echo "  \"restarts_before\": \"$RESTARTS_BEFORE\""
    echo "}"
} > "$META"

echo "soak run -> $OUT_DIR"
echo "  duration=${DURATION_H}h interval=${INTERVAL}s core=$CORE_URL unit=$RUNTIME_UNIT"

# Pre-flight: genie-core must answer /api/health before we commit to a soak.
if command -v curl > /dev/null 2>&1; then
    if ! curl -fsS --max-time 10 "$CORE_URL/api/health" > /dev/null 2>&1; then
        echo "ERROR: genie-core not reachable at $CORE_URL/api/health" >&2
        exit 1
    fi
fi

# Start tegrastats if present (Jetson-only).
if command -v tegrastats > /dev/null 2>&1; then
    tegrastats --interval "$TEGRASTATS_INTERVAL_MS" --logfile "$TEGRASTATS_LOG" &
    TEGRASTATS_PID="$!"
    echo "  tegrastats pid=$TEGRASTATS_PID -> $TEGRASTATS_LOG"
else
    echo "  tegrastats not found — skipping GPU/RAM trace"
fi

DRIVER_ARGS=(
    "$SCRIPT_DIR/soak_driver.py"
    --core-url "$CORE_URL"
    --interval "$INTERVAL"
    --duration "$DURATION_S"
    --out "$TELEMETRY"
    --runtime-unit "$RUNTIME_UNIT"
)
if [ -n "$PROMPT" ]; then
    DRIVER_ARGS+=(--prompt "$PROMPT")
fi

echo "  driving for ${DURATION_S}s ..."
"$PY" "${DRIVER_ARGS[@]}"

# Stop the trace and capture post-run state.
cleanup
TEGRASTATS_PID=""
RESTARTS_AFTER="$(runtime_restarts)"

# Capture kernel + service logs since the run started for the OOM scan.
{
    if command -v journalctl > /dev/null 2>&1; then
        journalctl --since "$START_ISO" --no-pager 2>/dev/null || true
    fi
    if command -v dmesg > /dev/null 2>&1; then
        dmesg 2>/dev/null || true
    fi
} > "$JOURNAL"

RESTARTS_DELTA_ARGS=()
if [ -n "$RESTARTS_BEFORE" ] && [ -n "$RESTARTS_AFTER" ]; then
    DELTA=$(( RESTARTS_AFTER - RESTARTS_BEFORE ))
    RESTARTS_DELTA_ARGS=(--restarts "$DELTA")
    echo "  runtime restarts: before=$RESTARTS_BEFORE after=$RESTARTS_AFTER delta=$DELTA"
fi

echo "  scoring ..."
set +e
"$PY" "$SCRIPT_DIR/analyze_soak.py" \
    --telemetry "$TELEMETRY" \
    --journal "$JOURNAL" \
    --duration-target-h "$DURATION_H" \
    --budget-p50-ms "$BUDGET_P50_MS" \
    --budget-p99-ms "$BUDGET_P99_MS" \
    --out-json "$SUMMARY" \
    --out-md "$REPORT" \
    "${RESTARTS_DELTA_ARGS[@]}"
RC=$?
set -e

echo ""
echo "report:  $REPORT"
echo "summary: $SUMMARY"
exit "$RC"
