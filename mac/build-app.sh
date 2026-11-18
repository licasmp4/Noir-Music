#!/bin/sh
# Monta dist/Jam.app (Mac com Apple Silicon). Precisa: brew install opus
set -e
cd "$(dirname "$0")/.."
export LIBOPUS_LIB_DIR=/opt/homebrew/opt/opus/lib LIBOPUS_STATIC=1
cargo build --release -p jam
APP=dist/Jam.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/Jam "$APP/Contents/MacOS/Jam"
cp mac/Info.plist "$APP/Contents/Info.plist"
# ícone a partir do png
ICON=$(mktemp -d)/Jam.iconset; mkdir -p "$ICON"
for s in 16 32 128 256; do
  sips -z $s $s app/assets/jam-256.png --out "$ICON/icon_${s}x${s}.png" >/dev/null
  d=$((s*2)); [ $d -le 256 ] && sips -z $d $d app/assets/jam-256.png --out "$ICON/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICON" -o "$APP/Contents/Resources/Jam.icns"
codesign --force --deep -s - "$APP"
(cd dist && rm -f Jam-Mac.zip && ditto -c -k --keepParent Jam.app Jam-Mac.zip)
echo "pronto: $APP e dist/Jam-Mac.zip"
