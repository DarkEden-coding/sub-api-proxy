#!/bin/sh
set -eu
source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
output=${1:-"$source_dir/../../../target/macos/Sub-Api-Proxy.app"}
mkdir -p "$output/Contents/MacOS"
mkdir -p "$output/Contents/Resources"
cp "$source_dir/Info.plist" "$output/Contents/Info.plist"
cp "$source_dir/AppIcon.icns" "$source_dir"/MenuBarIconTemplate*.png "$output/Contents/Resources/"
perl -e 'alarm 180; exec @ARGV' xcrun swiftc -O -swift-version 5 -parse-as-library \
    -target "$(uname -m)-apple-macosx13.0" -framework AppKit \
    "$source_dir/MenuBar.swift" -o "$output/Contents/MacOS/Sub-Api-Proxy"
codesign --force --sign - "$output"
plutil -lint "$output/Contents/Info.plist"
printf 'Built %s\n' "$output"
