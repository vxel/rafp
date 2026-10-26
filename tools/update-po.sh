#!/usr/bin/env bash
# Extract the translatable strings into po/rafp.pot and merge them into
# every translation listed in po/LINGUAS.
set -euo pipefail
cd "$(dirname "$0")/.."
grep '\.rs$' po/POTFILES.in > po/.rust-files
xgettext --from-code=UTF-8 --language=Rust --add-comments=Translators \
    --keyword=gettext --keyword=gettext_f --keyword=ngettext:1,2 --keyword=ngettext_f:1,2 \
    --keyword=pgettext:1c,2 --package-name=rafp --package-version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)" \
    --msgid-bugs-address= \
    --files-from=po/.rust-files -o po/rafp.pot
xgettext --join-existing --language=Desktop --add-comments=Translators --msgid-bugs-address= \
    --package-name=rafp --package-version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)" \
    -o po/rafp.pot data/org.delaunois.Rafp.desktop.in
xgettext --join-existing --its=/usr/share/gettext/its/shared-mime-info.its --msgid-bugs-address= \
    --package-name=rafp --package-version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)" \
    -o po/rafp.pot data/org.delaunois.Rafp.mime.xml.in
rm po/.rust-files
while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    if [[ -f "po/$lang.po" ]]; then
        msgmerge --quiet --update --backup=none "po/$lang.po" po/rafp.pot
    else
        msginit --no-translator --locale="$lang" -i po/rafp.pot -o "po/$lang.po"
    fi
done < po/LINGUAS
echo "Updated po/rafp.pot and $(grep -cv '^#' po/LINGUAS) translation(s)."
