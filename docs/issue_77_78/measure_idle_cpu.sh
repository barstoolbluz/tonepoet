#!/usr/bin/env bash
# Measure CPU as a percentage of one core in an already-running safe TUI.
set -euo pipefail
if (( $# < 1 || $# > 2 )); then
    echo 'usage: measure_idle_cpu.sh TUI_PID [SECONDS=20]' >&2
    exit 2
fi
pid=$1
seconds=${2:-20}
if [[ ! "$pid" =~ ^[0-9]+$ ]] || [[ ! "$seconds" =~ ^[0-9]+$ ]] || (( seconds < 1 )); then
    echo 'PID and seconds must be positive integers' >&2
    exit 2
fi
hz=$(getconf CLK_TCK)
get_sample() {
    local stat rest
    stat=$(<"/proc/$pid/stat") || return
    rest=${stat##*) }
    local -a fields
    read -r -a fields <<< "$rest"
    # The process start tick guards against measuring a reused PID.
    printf '%s %s %s\n' "${fields[11]}" "${fields[12]}" "${fields[19]}"
}
read -r user0 sys0 start0 < <(get_sample)
sleep "$seconds"
read -r user1 sys1 start1 < <(get_sample)
if [[ "$start0" != "$start1" ]]; then
    echo 'PID was reused during the measurement window' >&2
    exit 1
fi
awk -v ticks="$((user1 + sys1 - user0 - sys0))" -v hz="$hz" -v sec="$seconds" \
    'BEGIN {printf "CPU: %.4f%% of one core (%.3f CPU seconds in %d wall seconds)\n", 100*ticks/hz/sec, ticks/hz, sec}'
