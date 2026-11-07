#!/usr/bin/env bash
# Real GPUI window launch smoke for macOS.
#
# 1. builds the Desktop with `gpui_platform/runtime_shaders` so no Metal
#    toolchain is required;
# 2. starts a real `ukk-host serve` on a private Unix socket + database;
# 3. launches the native Desktop binary and asserts, via CGWindowList, that the
#    process owns a real layer-0 window with non-zero bounds;
# 4. terminates the Desktop and proves the Host is still alive and serving.
#
# Every wait is bounded. All child processes are cleaned up on any exit path.
# Set UKK_SMOKE_BUILD=0 to reuse an existing build.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/ukk-gpui-smoke-macos.XXXXXX")
LOG_DIR="${UKK_SMOKE_LOG_DIR:-$ROOT/target/launch-smoke/macos}"
mkdir -p "$LOG_DIR"
HOST_PID=""
APP_PID=""
HOST_BIN="${UKK_HOST_BIN:-$ROOT/target/debug/ukk-host}"
APP_BIN="${UKK_DESKTOP_BIN:-$ROOT/apps/desktop/target/debug/ukk-desktop}"

cleanup() {
    status=$?
    for pid in "$APP_PID" "$HOST_PID"; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
            for _ in $(seq 1 50); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.1
            done
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done
    rm -rf "$WORK"
    if [ "$status" -ne 0 ]; then
        echo "failure logs retained in $LOG_DIR" >&2
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM HUP

# Runs a command with a hard deadline (macOS has no coreutils `timeout`).
run_with_deadline() {
    local seconds=$1
    shift
    "$@" &
    local pid=$!
    local ticks=$((seconds * 10))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$ticks" -le 0 ]; then
            kill -KILL "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            return 124
        fi
        sleep 0.1
        ticks=$((ticks - 1))
    done
    wait "$pid"
}

if [ "${UKK_SMOKE_BUILD:-1}" = "1" ]; then
    echo "==> Building ukk-host"
    cargo build --manifest-path "$ROOT/Cargo.toml" -p ukk-host
    echo "==> Building ukk-desktop with runtime shaders"
    cargo build --manifest-path "$ROOT/apps/desktop/Cargo.toml" \
        --features gpui_platform/runtime_shaders
fi

for bin in "$HOST_BIN" "$APP_BIN"; do
    if [ ! -x "$bin" ]; then
        echo "error: missing executable $bin" >&2
        exit 1
    fi
done

echo "==> Compiling the CGWindowList probe"
PROBE="$WORK/window_probe"
xcrun swiftc -O "$ROOT/scripts/ci/window_probe.swift" -o "$PROBE"

# Private socket and database: nothing touches the developer profile.
export UKK_SOCKET="$WORK/agent-host.sock"
export UKK_DB_PATH="$WORK/workspace.sqlite3"

echo "==> Starting ukk-host on $UKK_SOCKET"
"$HOST_BIN" serve >"$LOG_DIR/host.log" 2>&1 &
HOST_PID=$!

for _ in $(seq 1 300); do
    [ -S "$UKK_SOCKET" ] && break
    if ! kill -0 "$HOST_PID" 2>/dev/null; then
        echo "error: Host exited before creating its socket" >&2
        cat "$LOG_DIR/host.log" >&2
        exit 1
    fi
    sleep 0.1
done
if [ ! -S "$UKK_SOCKET" ]; then
    echo "error: Host socket was not created within 30s" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi

echo "==> Checking the Host answers host.ping"
if ! run_with_deadline 10 "$HOST_BIN" request host.ping >"$WORK/ping.json" 2>"$WORK/ping.err"; then
    echo "error: host.ping failed before Desktop launch" >&2
    cat "$WORK/ping.err" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi
cat "$WORK/ping.json"

echo "==> Launching ukk-desktop (pid will be reported)"
"$APP_BIN" >"$LOG_DIR/app.log" 2>&1 &
APP_PID=$!
echo "Desktop pid: $APP_PID"

WINDOW_JSON="$LOG_DIR/window.json"
WINDOW_FOUND=0
deadline=$((SECONDS + 60))
while [ "$SECONDS" -lt "$deadline" ]; do
    if ! kill -0 "$APP_PID" 2>/dev/null; then
        echo "error: Desktop exited before opening a window" >&2
        cat "$LOG_DIR/app.log" >&2
        exit 1
    fi
    if "$PROBE" "$APP_PID" >"$WINDOW_JSON" 2>"$WORK/probe.err"; then
        WINDOW_FOUND=1
        break
    fi
    sleep 0.25
done

if [ "$WINDOW_FOUND" -ne 1 ]; then
    echo "error: no CoreGraphics layer-0 window for pid $APP_PID within 60s" >&2
    echo "--- launchctl managername ---" >&2
    launchctl managername >&2 || true
    echo "--- probe ---" >&2
    cat "$WINDOW_JSON" >&2 || true
    cat "$WORK/probe.err" >&2 || true
    echo "--- app log ---" >&2
    cat "$LOG_DIR/app.log" >&2
    exit 1
fi

echo "==> Window evidence:"
cat "$WINDOW_JSON"

echo "==> Terminating the Desktop"
kill -TERM "$APP_PID"
for _ in $(seq 1 100); do
    kill -0 "$APP_PID" 2>/dev/null || break
    sleep 0.1
done
if kill -0 "$APP_PID" 2>/dev/null; then
    echo "error: Desktop ignored SIGTERM for 10s" >&2
    exit 1
fi
wait "$APP_PID" 2>/dev/null || true
APP_PID=""

echo "==> Asserting the Host survived Desktop termination"
if ! kill -0 "$HOST_PID" 2>/dev/null; then
    echo "error: Host died when the Desktop process terminated" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi
if ! run_with_deadline 10 "$HOST_BIN" request host.ping >"$WORK/ping-after.json" 2>"$WORK/ping-after.err"; then
    echo "error: Host stopped answering host.ping after Desktop termination" >&2
    cat "$WORK/ping-after.err" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi
cat "$WORK/ping-after.json"

if pgrep -f "$APP_BIN" >/dev/null 2>&1; then
    echo "error: a ukk-desktop process is still running after termination" >&2
    pgrep -fl "$APP_BIN" >&2 || true
    exit 1
fi

echo "PASS: macOS GPUI window created and destroyed; Host stayed alive."
