#!/usr/bin/env bash
# Internal helper: run only inside the test's private Xvfb + session bus.
set -euo pipefail
: "${SNEKKIE_SMOKE_LOGS:?Set a smoke-test output directory}"
"$@" > "$SNEKKIE_SMOKE_LOGS/launch.log" 2>&1 &
child=$!
cleanup() {
    kill "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
}
trap cleanup EXIT
window=
for ((attempt = 0; attempt < 120; attempt++)); do
    if ! kill -0 "$child" 2>/dev/null; then
        cat "$SNEKKIE_SMOKE_LOGS/launch.log" >&2
        echo 'Snekkie exited before mapping a desktop window.' >&2
        exit 1
    fi
    window=$(xdotool search --onlyvisible --name '^Snekkie$' 2>/dev/null | head -n 1 || true)
    [[ -n $window ]] && break
    sleep 0.25
done
if [[ -z $window ]]; then
    cat "$SNEKKIE_SMOKE_LOGS/launch.log" >&2
    echo 'Snekkie did not map a desktop window within 30 seconds.' >&2
    exit 1
fi
xdotool getwindowgeometry --shell "$window" > "$SNEKKIE_SMOKE_LOGS/window.txt"
# Require the renderer/event loop to survive after mapping, catching delayed
# graphics initialization failures that a nonempty process list would miss.
sleep 3
kill -0 "$child"
xdotool search --onlyvisible --name '^Snekkie$' >/dev/null
if command -v import >/dev/null; then
    import -window "$window" "$SNEKKIE_SMOKE_LOGS/window.png"
fi
printf 'PASS: Snekkie mapped a window and stayed running. Logs: %s\n' "$SNEKKIE_SMOKE_LOGS"
