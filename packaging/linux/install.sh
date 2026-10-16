#!/bin/sh
# Installs this openADE for the current user: the files under
# ~/.local/opt/openade, the `openade` command in ~/.local/bin and an entry in
# the application menu. Run it again to update; `install.sh --uninstall`
# removes what it installed.
set -eu
here=$(dirname "$(readlink -f "$0")")
data=${XDG_DATA_HOME:-$HOME/.local/share}
target=$HOME/.local/opt/openade
bin=$HOME/.local/bin
# Write permission first, so a read-only copy can still be replaced or removed.
remove() { [ -e "$1" ] && chmod -R u+w "$1"; rm -rf "$1"; }
if [ "${1:-}" = "--uninstall" ]; then
    remove "$target"
    rm -f "$bin/openade" "$data/applications/openade.desktop" "$data/icons/hicolor/scalable/apps/openade.svg"
    echo "openADE removed. Your projects and the data in ~/.local/share/openide stay."
    exit 0
fi
if [ "$here" != "$target" ]; then
    remove "$target.new"
    mkdir -p "$(dirname "$target")"
    cp -a "$here" "$target.new"
    remove "$target"
    mv "$target.new" "$target"
fi
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/scalable/apps"
ln -sf "$target/openade" "$bin/openade"
cp "$target/openade.svg" "$data/icons/hicolor/scalable/apps/openade.svg"
sed "s|^Exec=.*|Exec=$target/openade|" "$target/openade.desktop" > "$data/applications/openade.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "openADE installed in $target."
case ":$PATH:" in
    *":$bin:"*) echo "Run it from the application menu or with: openade" ;;
    *) echo "Run it from the application menu or with: $bin/openade" ;;
esac
