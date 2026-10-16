#!/bin/sh
# Builds the Linux archive of a release from a built binary and an unpacked
# CEF directory (CEF_PATH): openADE-<version>-linux-x86_64.tar.gz in <out>.
# Usage: scripts/package-linux.sh <agents-window> <cef-dir> <version> <out>
set -eu
binary=$1 cef=$2 version=$3 out=$4
root=$(dirname "$(readlink -f "$0")")/..
name=openADE-$version-linux-x86_64
stage=$out/$name
rm -rf "$stage"
mkdir -p "$stage/cef/locales"
cp "$binary" "$stage/agents-window"
strip "$stage/agents-window"
# What CEF loads at run time; the headers and CMake files are only for building.
for file in "$cef"/*.so "$cef"/*.so.* "$cef"/*.bin "$cef"/*.dat "$cef"/*.pak "$cef"/*.json "$cef"/chrome-sandbox; do
    [ -e "$file" ] && cp -a "$file" "$stage/cef/"
done
cp -a "$cef"/locales/. "$stage/cef/locales/"
# Chromium's own Vulkan loader carries the system's name; the window's
# renderer must open the system's loader, so it is left out (docs/browser-cef.md).
rm -f "$stage/cef/libvulkan.so.1" "$stage/cef/archive.json"
for file in libcef.so icudtl.dat v8_context_snapshot.bin resources.pak locales/en-US.pak; do
    [ -e "$stage/cef/$file" ] || { echo "missing cef/$file" >&2; exit 1; }
done
cp "$root/packaging/linux/openade" "$root/packaging/linux/install.sh" "$root/packaging/linux/openade.desktop" "$stage/"
cp "$root/apps/agents-window/assets/brand/openide.svg" "$stage/openade.svg"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/THIRD_PARTY_NOTICES.md" "$stage/"
[ -e "$cef/CREDITS.html" ] && cp "$cef/CREDITS.html" "$stage/cef/CREDITS.html"
chmod +x "$stage/openade" "$stage/install.sh"
# Files copied from a read-only store stay removable once installed.
chmod -R u+w "$stage"
tar -C "$out" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
