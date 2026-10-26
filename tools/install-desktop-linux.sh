#!/usr/bin/env bash
# Build rafp and add it to the desktop application menu (current user only):
# the binary in ~/.local/bin, its translations in ~/.local/share/locale, its
# icons in ~/.local/share/icons, the AFP file type in ~/.local/share/mime,
# and a translated .desktop file named after the application id.
set -euo pipefail
cd "$(dirname "$0")/.."
prefix="$HOME/.local"
cargo build --release -p afp-viewer
install -Dm755 target/release/rafp "$prefix/bin/rafp"
while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    install -d "$prefix/share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$prefix/share/locale/$lang/LC_MESSAGES/rafp.mo" "po/$lang.po"
done < po/LINGUAS
icons="$prefix/share/icons/hicolor"
install -Dm644 data/icons/org.delaunois.Rafp.svg "$icons/scalable/apps/org.delaunois.Rafp.svg"
install -Dm644 data/icons/org.delaunois.Rafp-symbolic.svg "$icons/symbolic/apps/org.delaunois.Rafp-symbolic.svg"
gtk4-update-icon-cache -qtf "$icons" 2>/dev/null || gtk-update-icon-cache -qtf "$icons" 2>/dev/null || true
mime="$prefix/share/mime"
install -d "$mime/packages"
msgfmt --xml -d po --template data/org.delaunois.Rafp.mime.xml.in -o "$mime/packages/org.delaunois.Rafp.xml"
update-mime-database "$mime" 2>/dev/null || true
apps="$prefix/share/applications"
install -d "$apps"
rm -f "$apps/rafp.desktop" "$apps/org.rafp.Rafp.desktop"  # earlier application ids
sed "s|@EXEC@|$prefix/bin/rafp|" data/org.delaunois.Rafp.desktop.in > "$apps/org.delaunois.Rafp.desktop.in"
msgfmt --desktop -d po --template "$apps/org.delaunois.Rafp.desktop.in" -o "$apps/org.delaunois.Rafp.desktop"
rm "$apps/org.delaunois.Rafp.desktop.in"
update-desktop-database "$apps" 2>/dev/null || true
echo "Installed: $prefix/bin/rafp (menu entry: rafp)"
