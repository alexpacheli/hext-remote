#!/bin/bash
# The Linux AppImage (F6), from the program:
#
#   installer/linux/appimage.sh dist/hext-remote 0.6.0 dist
#
# makes dist/HextRemote-0.6.0-x86_64.AppImage. Needs appimagetool on the PATH.
set -euo pipefail

program="$1"
version="$2"
out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

appdir="$(mktemp -d)/HextRemote.AppDir"
mkdir -p "$appdir/usr/bin"
cp "$program" "$appdir/usr/bin/hext-remote"
chmod 755 "$appdir/usr/bin/hext-remote"
cp "$here/hext-remote.desktop" "$appdir/hext-remote.desktop"
cp "$root/apps/hext/icons/icon.png" "$appdir/hext-remote.png"
ln -s usr/bin/hext-remote "$appdir/AppRun"
ARCH=x86_64 VERSION="$version" appimagetool --no-appstream "$appdir" \
  "$out/HextRemote-$version-x86_64.AppImage"
