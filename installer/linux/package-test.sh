#!/bin/bash
# Inside a clean container (Ubuntu or Fedora), as on a user's machine:
# installs the package alone, checks that it brought what the program opens,
# runs the interface and the viewer (ui-smoke.sh), and removes it.
#
#   docker run --rm -v "$PWD:/src:ro" -v "$PWD/dist:/dist" ubuntu:24.04 \
#     bash /src/installer/linux/package-test.sh /dist/hext-remote_X_amd64.deb /dist/shots
set -euo pipefail

package="$1"
out="$2"
here="$(cd "$(dirname "$0")" && pwd)"

case "$package" in
  *.deb)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq "$package" >/dev/null
    tools=(xvfb x11-utils xdotool imagemagick dbus)
    install_tools() { apt-get install -y -qq --no-install-recommends "${tools[@]}" >/dev/null; }
    remove() { apt-get remove -y -qq hext-remote >/dev/null; }
    ;;
  *.rpm)
    dnf install -y -q "$package" >/dev/null
    tools=(xorg-x11-server-Xvfb xdpyinfo xprop xdotool ImageMagick dbus-daemon)
    install_tools() { dnf install -y -q "${tools[@]}" >/dev/null; }
    remove() { dnf remove -y -q hext-remote >/dev/null; }
    ;;
  *) echo "pacote desconhecido: $package"; exit 2 ;;
esac

hext-remote --version

# What the viewer's window opens only when it starts (winit: the keyboard on
# X11; wgpu: Vulkan, or GL through EGL), so no automatic list finds it: the
# package must have brought it. Checked before the test's own tools come in.
missing=0
for lib in libxkbcommon-x11.so.0 libvulkan.so.1 libEGL.so.1; do
  if ! ldconfig -p | grep -q "$lib"; then
    echo "o pacote não trouxe $lib"; missing=1
  fi
done
[ "$missing" = 0 ]

# The host's unit (F7), for each user to turn on (`service install`).
unit=/usr/lib/systemd/user/hext-remote.service
grep -qx 'ExecStart=/usr/bin/hext-remote ui' "$unit" || { echo "o pacote não trouxe $unit"; exit 1; }

# Our apt/dnf repository: subscribed only when the tree has its key
# (repo.sh; the whole path is in repo-test.sh).
case "$package" in
  *.deb) subscription=/etc/apt/sources.list.d/hext-remote.sources ;;
  *.rpm) subscription=/etc/yum.repos.d/hext-remote.repo ;;
esac
if [ -f "$here/repo-key.asc" ]; then
  [ -f "$subscription" ] || { echo "o pacote não inscreveu o repositório ($subscription)"; exit 1; }
else
  [ ! -e "$subscription" ] || { echo "sem repo-key.asc, o pacote não deveria trazer $subscription"; exit 1; }
fi

# The interface is a host now (F7): kept off our signaling server and
# relay, so a test run registers no computer there.
echo "127.0.0.1 signal.hext.com.br relay.hext.com.br" >> /etc/hosts

install_tools
bash "$here/ui-smoke.sh" /usr/bin/hext-remote "$out"
remove
[ ! -e "$subscription" ] || { echo "remover deixou $subscription"; exit 1; }
echo "ok: $(basename "$package")"
