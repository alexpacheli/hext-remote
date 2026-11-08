#!/bin/bash
# The macOS app and disk image (F6), from the universal program:
#
#   installer/macos/bundle.sh dist/HextRemote 0.6.0 dist
#
# makes "dist/Hext Remote.app" and "dist/HextRemote-0.6.0.dmg".
#
# Signing and notarization happen only when their settings are there (the
# release pipeline takes them from the repository's secrets):
#   APPLE_SIGNING_IDENTITY  "Developer ID Application: Name (TEAMID)", in a
#                           keychain already unlocked; without it, signed ad
#                           hoc (runs only after the quarantine is removed)
#   APPLE_ID, APPLE_APP_PASSWORD, APPLE_TEAM_ID
#                           for notarytool; without them, not notarized
set -euo pipefail

program="$1"
version="$2"
out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"

app="$out/Hext Remote.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$program" "$app/Contents/MacOS/HextRemote"
chmod 755 "$app/Contents/MacOS/HextRemote"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

# The icon, in every size the system asks for.
iconset="$(mktemp -d)/HextRemote.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size "$root/apps/hext/icons/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double "$root/apps/hext/icons/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/HextRemote.icns"

if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  echo "Assinando com $APPLE_SIGNING_IDENTITY"
  codesign --force --options runtime --timestamp \
    --entitlements "$here/entitlements.plist" \
    --sign "$APPLE_SIGNING_IDENTITY" "$app"
else
  echo "Sem certificado: assinatura ad hoc (o Mac avisa ao abrir)"
  codesign --force --sign - --identifier br.com.hext.remote "$app"
fi
codesign --verify --deep --strict "$app"

dmg="$out/HextRemote-$version.dmg"
staging="$(mktemp -d)"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
hdiutil create -volname "Hext Remote" -srcfolder "$staging" -ov -format UDZO "$dmg"

if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$dmg"
fi

if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ -n "${APPLE_ID:-}" ] \
  && [ -n "${APPLE_APP_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  echo "Notarizando"
  xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait
  xcrun stapler staple "$dmg"
  xcrun stapler staple "$app"
else
  echo "Sem as credenciais da Apple: não notarizado"
fi

# For the automatic update (F6, marco 2): the app alone, compressed.
tar -C "$out" -czf "$out/HextRemote-$version-macos.app.tar.gz" "Hext Remote.app"
