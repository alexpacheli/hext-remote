#!/bin/bash
# Opens the interface on a virtual screen and checks that its window shows
# up; leaves a picture of it in the folder given (CI keeps it):
#
#   installer/linux/ui-smoke.sh dist/hext-remote shots
#
# Needs xvfb, xdotool, imagemagick and dbus (dbus-run-session).
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

Xvfb :99 -screen 0 1280x800x24 >/dev/null 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
export DISPLAY=:99
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.2; done

dbus-run-session -- "$program" >"$out/ui.log" 2>&1 &
app=$!

window=""
for _ in $(seq 60); do
  if ! kill -0 "$app" 2>/dev/null; then
    echo "a interface fechou sozinha:"; cat "$out/ui.log"; exit 1
  fi
  window="$(xdotool search --name '^Hext Remote$' 2>/dev/null | head -1 || true)"
  [ -n "$window" ] && break
  sleep 0.5
done
if [ -z "$window" ]; then
  echo "a janela do Hext Remote não apareceu:"; cat "$out/ui.log"; kill "$app" || true; exit 1
fi

# Time for the page to draw.
sleep 5
import -window root "$out/home.png"
kill -0 "$app" || { echo "a interface fechou:"; cat "$out/ui.log"; exit 1; }
kill "$app" || true
echo "janela $window: $out/home.png"

# The viewer's window, toward an ID nobody has: it must open and say so,
# not fall over a library it opens only when it starts.
timeout 30 "$program" view 123456789 >"$out/viewer.log" 2>&1 || true
if grep -q panicked "$out/viewer.log"; then
  echo "o visualizador caiu:"; cat "$out/viewer.log"; exit 1
fi
grep -v INFO "$out/viewer.log" | tail -3
