#!/usr/bin/env bash
# Build each base image and run docker/test.sh inside it; print a pass/fail table.
#
#   docker/matrix.sh                      # all bases
#   docker/matrix.sh ubuntu:24.04 cachy   # a subset (exact name or substring)
#
# Logs go to target/docker-logs/<slug>.log. Builds are cached in named volumes:
# janus-scan-target-<slug> (CARGO_TARGET_DIR, one per image) and
# janus-scan-cargo-registry (shared ~/.cargo/registry).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOGS="$ROOT/target/docker-logs"
mkdir -p "$LOGS"

# base image -> Dockerfile. Ubuntu 24.04 also stands in for Linux Lite 7.
ALL_BASES=(
    "ubuntu:24.04|apt"
    "linuxmintd/mint22.3-amd64|apt"
    # Baseline CachyOS image (github.com/CachyOS/docker, plain Dockerfile).
    "cachyos/cachyos:latest|pacman"
)

selected=()
if [ $# -eq 0 ]; then
    selected=("${ALL_BASES[@]}")
else
    for arg in "$@"; do
        found=0
        for entry in "${ALL_BASES[@]}"; do
            base="${entry%%|*}"
            if [ "$base" = "$arg" ] || [[ "$base" == *"$arg"* ]]; then
                selected+=("$entry"); found=1
            fi
        done
        [ $found = 1 ] || { echo "unknown base: $arg" >&2; exit 2; }
    done
fi

slug() { echo "$1" | tr '/:.' '---'; }

declare -a results
for entry in "${selected[@]}"; do
    base="${entry%%|*}"; kind="${entry##*|}"; s="$(slug "$base")"
    tag="janus-scan-test:$s"; log="$LOGS/$s.log"
    start=$(date +%s)
    echo ">>> $base: building $tag (log: $log)"
    if ! docker build --platform linux/amd64 --build-arg "BASE=$base" \
            -f "$ROOT/docker/$kind.Dockerfile" -t "$tag" "$ROOT" >"$log" 2>&1; then
        results+=("$base|FAIL (image build)|$(( $(date +%s) - start ))s")
        continue
    fi
    echo ">>> $base: running test.sh"
    if docker run --rm --platform linux/amd64 \
            -v "$ROOT:/src:ro" \
            -v "janus-scan-target-$s:/target" \
            -v "janus-scan-cargo-registry:/usr/local/cargo/registry" \
            -e CARGO_TARGET_DIR=/target \
            -e CARGO_TERM_COLOR=never \
            "$tag" >>"$log" 2>&1; then
        status="pass"
    else
        status="FAIL (test.sh)"
    fi
    results+=("$base|$status|$(( $(date +%s) - start ))s")
done

echo
printf '%-30s %-20s %s\n' BASE RESULT TIME
printf '%-30s %-20s %s\n' ---- ------ ----
fail=0
for r in "${results[@]}"; do
    IFS='|' read -r b st t <<<"$r"
    printf '%-30s %-20s %s\n' "$b" "$st" "$t"
    [ "$st" = pass ] || fail=1
done
exit $fail
