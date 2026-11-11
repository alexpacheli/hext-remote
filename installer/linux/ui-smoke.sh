#!/bin/bash
# Opens the interface on a virtual screen and checks that its window shows
# up; then the viewer, with video from the test pattern host on this same
# machine (by IP, no server). Leaves pictures of both in the folder given:
#
#   installer/linux/ui-smoke.sh dist/hext-remote shots
#
# Needs xvfb, xdpyinfo, xdotool, imagemagick and dbus-daemon. It brings none
# of the program's libraries: what it runs with is what is installed.
set -euo pipefail

program="$(realpath "$1")"
out="$2"
mkdir -p "$out"
out="$(realpath "$out")"

# A user of its own: no key, account or list from anyone.
home="$(mktemp -d)"
export HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share"
# (No GPU on the virtual screen.)
export WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1

pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT

Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 &
pids+=($!)
export DISPLAY=:99
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.2; done

# A session bus of its own (the single-instance lock lives there).
bus="$(dbus-daemon --session --fork --print-address=1 --print-pid=1)"
DBUS_SESSION_BUS_ADDRESS="$(sed -n 1p <<<"$bus")"
export DBUS_SESSION_BUS_ADDRESS
pids+=("$(sed -n 2p <<<"$bus")")

# Waits up to $2 half-seconds for a window named like $1, while $3 runs.
window_of() {
  for _ in $(seq "$2"); do
    if ! kill -0 "$3" 2>/dev/null; then return 1; fi
    local found
    found="$(xdotool search --name "$1" 2>/dev/null | head -1 || true)"
    [ -n "$found" ] && { echo "$found"; return 0; }
    sleep 0.5
  done
  return 1
}

# ---- The interface ----
"$program" >"$out/ui.log" 2>&1 &
app=$!
pids+=("$app")
if ! window="$(window_of '^Hext Remote$' 60 "$app")"; then
  echo "a janela do Hext Remote não apareceu:"; cat "$out/ui.log"; exit 1
fi
sleep 5 # for the page to draw
import -window root "$out/home.png"
kill -0 "$app" || { echo "a interface fechou:"; cat "$out/ui.log"; exit 1; }
echo "interface: janela $window ($(xprop -id "$window" WM_CLASS 2>/dev/null || true))"
kill "$app"; wait "$app" 2>/dev/null || true

# ---- The viewer, with video ----
"$program" host --server off --relay off --code 424242 --listen 127.0.0.1:7420 \
  --device-key "$home/host.key" >"$out/host.log" 2>&1 &
host=$!
pids+=("$host")
key=""
for _ in $(seq 40); do
  key="$(grep -o '[0-9a-f]\{64\}' "$out/host.log" | head -1 || true)"
  [ -n "$key" ] && break
  sleep 0.5
done
[ -n "$key" ] || { echo "o host de teste não abriu:"; cat "$out/host.log"; exit 1; }

"$program" view 127.0.0.1 --code 424242 --key "$key" --relay off \
  --device-key "$home/viewer.key" >"$out/viewer.log" 2>&1 &
viewer=$!
pids+=("$viewer")
# The title shows the frame rate once video is drawn.
if ! window="$(window_of 'fps' 60 "$viewer")"; then
  echo "o visualizador não mostrou vídeo:"; cat "$out/viewer.log"; exit 1
fi
sleep 2
import -window root "$out/video.png"
echo "visualizador: $(xdotool getwindowname "$window") ($(xprop -id "$window" WM_CLASS 2>/dev/null || true))"
