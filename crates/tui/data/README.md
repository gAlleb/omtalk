# Emoji catalog

`emoji.tsv` contains 375 everyday fully-qualified sequences from Unicode Emoji 17.0,
selected in `common-emoji.txt` without skin-tone or gender duplicates,
ordered by the Unicode catalog, with Russian CLDR 48 names and search keywords.
Columns: group index, emoji sequence, English name, Russian name, Russian keywords.
All data is included in the binary; the picker does not access the network.

Sources (Unicode, Inc.; see `UNICODE-LICENSE.txt`):

- https://www.unicode.org/Public/17.0.0/emoji/emoji-test.txt
- https://github.com/unicode-org/cldr/blob/release-48/common/annotations/ru.xml
- https://github.com/unicode-org/cldr/blob/release-48/common/annotationsDerived/ru.xml

Regenerate with `generate-emoji.py` and local copies of these three files.
