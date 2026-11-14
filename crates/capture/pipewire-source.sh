#!/bin/bash
# Runs a command next to a PipeWire of its own with a test video source
# (GStreamer's SMPTE bars, 320x240), for the PipeWire capture test; the
# source's node id goes in HEXT_TEST_PIPEWIRE_NODE:
#
#   crates/capture/pipewire-source.sh cargo test -p hext-capture -- --ignored pipewire_
#
# Needs pipewire, wireplumber, dbus, gstreamer1.0-pipewire and
# gstreamer1.0-plugins-base (videotestsrc).
set -euo pipefail

XDG_RUNTIME_DIR="$(mktemp -d)"
export XDG_RUNTIME_DIR
chmod 700 "$XDG_RUNTIME_DIR"
logs="$XDG_RUNTIME_DIR/logs"
mkdir "$logs"
DBUS_SESSION_BUS_ADDRESS="$(dbus-daemon --session --fork --print-address=1)"
export DBUS_SESSION_BUS_ADDRESS

pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT
pipewire >"$logs/pipewire.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/pipewire-0" ] && break; sleep 0.1; done
wireplumber >"$logs/wireplumber.log" 2>&1 &
pids+=($!)
gst-launch-1.0 -q videotestsrc pattern=smpte is-live=true \
  ! video/x-raw,format=BGRx,width=320,height=240,framerate=30/1 \
  ! pipewiresink mode=provide stream-properties="p,media.class=Video/Source,node.name=hext-test" \
  >"$logs/gst.log" 2>&1 &
pids+=($!)

node=""
for _ in $(seq 40); do
  node="$(pw-cli ls Node 2>/dev/null | awk '/^\tid /{id=$2} /node.name = "hext-test"/{sub(",", "", id); print id}')"
  [ -n "$node" ] && break
  sleep 0.25
done
if [ -z "$node" ]; then
  echo "a fonte de teste do PipeWire não apareceu"
  tail -n 20 "$logs"/*.log
  exit 1
fi
echo "fonte de teste do PipeWire: nó $node"
HEXT_TEST_PIPEWIRE_NODE="$node" "$@"
