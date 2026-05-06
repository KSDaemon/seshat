#!/usr/bin/env bash
# Memory/FD/thread sampler for all live `seshat serve` processes.
#
# Writes JSONL summary to $LOG_DIR/summary.jsonl and full vmmap dumps to
# $LOG_DIR/vmmap-<pid>-<unix_ts>.txt every VMMAP_EVERY iterations.
#
# Usage:
#   tools/monitor-seshat.sh                # default: every 30s, log to ~/.seshat/monitor
#   INTERVAL=15 tools/monitor-seshat.sh    # every 15s
#   LOG_DIR=/tmp/seshat-mon tools/monitor-seshat.sh
#
# Run in background:
#   nohup tools/monitor-seshat.sh > /tmp/seshat-monitor.out 2>&1 &
#
# Inspect:
#   tail -f ~/.seshat/monitor/summary.jsonl
#   ls -la ~/.seshat/monitor/vmmap-*.txt

set -uo pipefail

# Force C locale so awk uses '.' as decimal separator (otherwise ru_RU produces "115,3"
# which is invalid JSON). macOS BSD awk reads LC_NUMERIC too, set both.
export LC_ALL=C
export LC_NUMERIC=C
unset LANG LC_CTYPE LC_MESSAGES LC_MONETARY LC_TIME LC_COLLATE 2>/dev/null || true

INTERVAL="${INTERVAL:-30}"
VMMAP_EVERY="${VMMAP_EVERY:-10}"   # full vmmap every N iterations (default ≈5 min at 30s)
LOG_DIR="${LOG_DIR:-$HOME/.seshat/monitor}"
mkdir -p "$LOG_DIR"
SUMMARY="$LOG_DIR/summary.jsonl"

echo "monitor-seshat started pid=$$ interval=${INTERVAL}s vmmap_every=${VMMAP_EVERY} log=$LOG_DIR" >&2

trap 'echo "monitor-seshat stopping (pid=$$)" >&2; exit 0' INT TERM

# Escape backslashes and double quotes for JSON string fields.
json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

# Extract physical-footprint token from `vmmap -summary` output.
# Returns the raw last token, e.g. "91.8G" or "12.5M" or "1234K". Empty → "".
parse_footprint() {
    local txt="$1"
    local line
    line=$(printf '%s\n' "$txt" | grep -m1 '^Physical footprint:' || true)
    [ -z "$line" ] && { echo ""; return; }
    printf '%s' "$line" | awk '{print $NF}'
}

# Extract VIRTUAL SIZE (raw token) and region count for a given MALLOC region.
# Lines look like:
#   MALLOC_LARGE                      30.6G   120.1M   ...     247         see ...
#   MALLOC_SMALL                      61.1G    12.3G   ...   15639         see ...
parse_malloc() {
    local txt="$1"
    local key="$2"
    local line
    line=$(printf '%s\n' "$txt" | grep -m1 "^${key}\b" || true)
    [ -z "$line" ] && { echo "  "; return; }
    local virtual count
    virtual=$(printf '%s' "$line" | awk '{print $2}')
    # Region-count column: last purely-numeric token on the line.
    count=$(printf '%s' "$line" | awk '{for(i=NF;i>=1;i--) if($i ~ /^[0-9]+$/){print $i; exit}}')
    echo "${virtual:-} ${count:-}"
}

iter=0
while :; do
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    pids=$(pgrep -f 'seshat serve' 2>/dev/null || true)

    if [ -z "$pids" ]; then
        printf '{"ts":"%s","alive":0}\n' "$ts" >> "$SUMMARY"
    else
        for pid in $pids; do
            ps_line=$(ps -o pid=,ppid=,rss=,vsz=,etime= -p "$pid" 2>/dev/null) || continue
            [ -z "$ps_line" ] && continue
            ppid=$(echo "$ps_line" | awk '{print $2}')
            rss=$(echo "$ps_line"  | awk '{print $3}')
            vsz=$(echo "$ps_line"  | awk '{print $4}')
            etime=$(echo "$ps_line" | awk '{print $5}')
            fds=$(lsof -p "$pid" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
            threads=$(ps -M -p "$pid" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
            cwd=$(lsof -p "$pid" 2>/dev/null | awk '$4=="cwd"{print $NF; exit}')
            parent_cmd=$(ps -o command= -p "$ppid" 2>/dev/null | head -c 200 || true)

            # Heavy: full vmmap summary every VMMAP_EVERY iterations.
            footprint=""
            mlarge=""; mlarge_n="null"
            msmall=""; msmall_n="null"
            if [ $((iter % VMMAP_EVERY)) -eq 0 ]; then
                vm_txt=$(vmmap -summary "$pid" 2>/dev/null || true)
                if [ -n "$vm_txt" ]; then
                    # save full dump for offline analysis
                    printf '%s' "$vm_txt" > "$LOG_DIR/vmmap-${pid}-$(date +%s).txt"
                    footprint=$(parse_footprint "$vm_txt")
                    read -r mlarge mlarge_n <<< "$(parse_malloc "$vm_txt" MALLOC_LARGE)"
                    read -r msmall msmall_n <<< "$(parse_malloc "$vm_txt" MALLOC_SMALL)"
                fi
            fi

            cwd_esc=$(json_escape "${cwd:-}")
            parent_esc=$(json_escape "${parent_cmd:-}")
            footprint_json='null'; [ -n "$footprint" ] && footprint_json="\"$footprint\""
            mlarge_json='null'; [ -n "$mlarge" ] && mlarge_json="\"$mlarge\""
            msmall_json='null'; [ -n "$msmall" ] && msmall_json="\"$msmall\""
            mlarge_n_json="${mlarge_n:-null}"; [ "$mlarge_n_json" = "" ] && mlarge_n_json="null"
            msmall_n_json="${msmall_n:-null}"; [ "$msmall_n_json" = "" ] && msmall_n_json="null"
            printf '{"ts":"%s","pid":%s,"ppid":%s,"rss_kb":%s,"vsz_kb":%s,"etime":"%s","fd":%s,"threads":%s,"footprint":%s,"malloc_large":%s,"malloc_large_n":%s,"malloc_small":%s,"malloc_small_n":%s,"cwd":"%s","parent_cmd":"%s"}\n' \
                "$ts" "$pid" "$ppid" "${rss:-0}" "${vsz:-0}" "$etime" "${fds:-0}" "${threads:-0}" \
                "$footprint_json" "$mlarge_json" "$mlarge_n_json" "$msmall_json" "$msmall_n_json" \
                "$cwd_esc" "$parent_esc" >> "$SUMMARY"
        done
    fi

    iter=$((iter + 1))
    sleep "$INTERVAL"
done
