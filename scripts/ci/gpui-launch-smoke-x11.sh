#!/usr/bin/env bash
# Real GPUI window launch smoke for Linux under X11 (Xvfb in CI).
#
# 1. builds the Desktop (runtime shaders are a no-op on Linux but keep the CI
#    build command identical to macOS);
# 2. starts a real `ukk-host serve` on a private Unix socket + database;
# 3. launches the native Desktop binary on $DISPLAY and asserts, via xdotool +
#    xwininfo, that the process owns a viewable X11 window with non-zero
#    geometry (GPUI sets _NET_WM_PID on its top-level window);
# 4. terminates the Desktop and proves the Host is still alive and serving.
#
# Run it through `xvfb-run -a -s "-screen 0 1280x800x24"`; the script refuses to
# run without a display server instead of silently going headless.
# Every wait is bounded. All child processes are cleaned up on any exit path.
# Set UKK_SMOKE_BUILD=0 to reuse an existing build.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
WORK=$(mktemp -d "/tmp/ukk-gpui-smoke-x11.XXXXXX")
LOG_DIR="${UKK_SMOKE_LOG_DIR:-$ROOT/target/launch-smoke/x11}"
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

if [ -z "${DISPLAY:-}" ]; then
    echo "error: DISPLAY is unset; run this script under xvfb-run or an X11 session" >&2
    exit 1
fi

# GPUI prefers Wayland when WAYLAND_DISPLAY is set, and starts headless under
# ZED_HEADLESS. This smoke asserts a real X11 window, so make the choice
# explicit instead of relying on the runner environment.
unset WAYLAND_DISPLAY ZED_HEADLESS
export XDG_RUNTIME_DIR="$WORK/runtime"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

for tool in xdotool xwininfo; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: $tool is required but not installed" >&2
        exit 1
    fi
done

if [ "${UKK_SMOKE_BUILD:-1}" = "1" ]; then
    echo "==> Building ukk-host"
    cargo build --manifest-path "$ROOT/Cargo.toml" -p ukk-host
    echo "==> Building ukk-desktop"
    cargo build --manifest-path "$ROOT/apps/desktop/Cargo.toml" \
        --features gpui_platform/runtime_shaders
fi

for bin in "$HOST_BIN" "$APP_BIN"; do
    if [ ! -x "$bin" ]; then
        echo "error: missing executable $bin" >&2
        exit 1
    fi
done

# Private socket and database: nothing touches the runner profile.
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
if ! "$HOST_BIN" request host.ping >"$WORK/ping.json" 2>"$WORK/ping.err"; then
    echo "error: host.ping failed before Desktop launch" >&2
    cat "$WORK/ping.err" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi
cat "$WORK/ping.json"

echo "==> Launching ukk-desktop on DISPLAY=$DISPLAY"
"$APP_BIN" >"$LOG_DIR/app.log" 2>&1 &
APP_PID=$!
echo "Desktop pid: $APP_PID"

WINDOW_ID=""
WINDOW_INFO="$LOG_DIR/window.info"
deadline=$((SECONDS + 60))
while [ "$SECONDS" -lt "$deadline" ]; do
    if ! kill -0 "$APP_PID" 2>/dev/null; then
        echo "error: Desktop exited before mapping a window" >&2
        cat "$LOG_DIR/app.log" >&2
        exit 1
    fi
    for id in $(xdotool search --pid "$APP_PID" 2>/dev/null || true); do
        if xwininfo -id "$id" >"$WINDOW_INFO" 2>/dev/null \
            && grep -q "Map State: IsViewable" "$WINDOW_INFO" \
            && awk '/Width:/ {w=$2} /Height:/ {h=$2} END {exit !(w > 0 && h > 0)}' "$WINDOW_INFO"; then
            WINDOW_ID=$id
            break
        fi
    done
    [ -n "$WINDOW_ID" ] && break
    sleep 0.25
done

if [ -z "$WINDOW_ID" ]; then
    echo "error: no viewable X11 window for pid $APP_PID within 60s" >&2
    echo "--- xdotool search (all window ids for pid) ---" >&2
    xdotool search --pid "$APP_PID" 2>&1 || true
    echo "--- xwininfo -root -tree ---" >&2
    xwininfo -root -tree 2>&1 | head -80 || true
    echo "--- xdpyinfo ---" >&2
    xdpyinfo 2>&1 | head -20 || true
    echo "--- app log ---" >&2
    cat "$LOG_DIR/app.log" >&2
    exit 1
fi

echo "==> Window evidence (id $WINDOW_ID):"
cat "$WINDOW_INFO"
echo "==> xdotool geometry:"
xdotool getwindowgeometry --shell "$WINDOW_ID"

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

if pgrep -f "$APP_BIN" >/dev/null 2>&1; then
    echo "error: a ukk-desktop process is still running after termination" >&2
    pgrep -fl "$APP_BIN" >&2 || true
    exit 1
fi

echo "==> Asserting the Host survived Desktop termination"
if ! kill -0 "$HOST_PID" 2>/dev/null; then
    echo "error: Host died when the Desktop process terminated" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi
if ! "$HOST_BIN" request host.ping >"$WORK/ping-after.json" 2>"$WORK/ping-after.err"; then
    echo "error: Host stopped answering host.ping after Desktop termination" >&2
    cat "$WORK/ping-after.err" >&2
    cat "$LOG_DIR/host.log" >&2
    exit 1
fi
cat "$WORK/ping-after.json"

echo "PASS: Linux X11 GPUI window mapped and destroyed; Host stayed alive."
