#!/usr/bin/env bash
# Build the Debian package of the viewer into target/debian/.
# Needs cargo-deb, gettext (msgfmt) and dpkg-dev (dpkg-shlibdeps).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --locked -p afp-viewer

# Files generated from data/ and po/, laid out as under /usr/share.
share=target/deb-assets/share
rm -rf target/deb-assets
install -d "$share/applications" "$share/mime/packages" \
    "$share/icons/hicolor/scalable/apps" "$share/icons/hicolor/symbolic/apps"
sed 's|@EXEC@|rafp|' data/org.delaunois.Rafp.desktop.in > target/deb-assets/rafp.desktop.in
msgfmt --desktop -d po --template target/deb-assets/rafp.desktop.in -o "$share/applications/org.delaunois.Rafp.desktop"
rm target/deb-assets/rafp.desktop.in
msgfmt --xml -d po --template data/org.delaunois.Rafp.mime.xml.in -o "$share/mime/packages/org.delaunois.Rafp.xml"
cp data/icons/org.delaunois.Rafp.svg "$share/icons/hicolor/scalable/apps/"
cp data/icons/org.delaunois.Rafp-symbolic.svg "$share/icons/hicolor/symbolic/apps/"
while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    install -d "$share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$share/locale/$lang/LC_MESSAGES/rafp.mo" "po/$lang.po"
done < po/LINGUAS

cargo deb -p afp-viewer --no-build "$@"
