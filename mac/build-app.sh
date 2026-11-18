#!/bin/sh
# Monta "dist/Noir Music.app" (Mac com Apple Silicon). Precisa: brew install opus
set -e
cd "$(dirname "$0")/.."
export LIBOPUS_LIB_DIR=/opt/homebrew/opt/opus/lib LIBOPUS_STATIC=1
cargo build --release -p noir-music
APP="dist/Noir Music.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/NoirMusic "$APP/Contents/MacOS/NoirMusic"
cp mac/Info.plist "$APP/Contents/Info.plist"
# ícone a partir do png
ICON=$(mktemp -d)/NoirMusic.iconset; mkdir -p "$ICON"
for s in 16 32 128 256; do
  sips -z $s $s app/assets/noir-music-256.png --out "$ICON/icon_${s}x${s}.png" >/dev/null
  d=$((s*2)); [ $d -le 256 ] && sips -z $d $d app/assets/noir-music-256.png --out "$ICON/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICON" -o "$APP/Contents/Resources/NoirMusic.icns"
codesign --force --deep -s - "$APP"
(cd dist && rm -f NoirMusic-Mac.zip && ditto -c -k --keepParent "Noir Music.app" NoirMusic-Mac.zip)
echo "pronto: $APP e dist/NoirMusic-Mac.zip"
