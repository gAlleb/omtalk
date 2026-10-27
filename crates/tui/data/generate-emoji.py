#!/usr/bin/env python3
"""Regenerate emoji.tsv from emoji-test.txt and CLDR ru annotation XML files.
Usage: python3 generate-emoji.py emoji-test.txt annotations/ru.xml annotationsDerived/ru.xml
Sources and license: README.md in this directory.
"""
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

names, keywords = {}, {}
normalize = lambda text: text.replace('\ufe0f', '')
for source in sys.argv[2:]:
    for item in ET.parse(source).iter('annotation'):
        key = normalize(item.attrib['cp'])
        value = ' '.join(''.join(item.itertext()).split())
        (names if item.attrib.get('type') == 'tts' else keywords)[key] = value
selected = {normalize(word) for line in Path(__file__).with_name('common-emoji.txt').read_text().splitlines() if not line.startswith('#') for word in line.split()}
rows, groups = [], []
for line in Path(sys.argv[1]).read_text().splitlines():
    if line.startswith('# group: '):
        group = line.removeprefix('# group: ')
    elif '; fully-qualified' in line:
        codes, detail = line.split(';', 1)
        symbol = ''.join(chr(int(code, 16)) for code in codes.split())
        name = detail.split('#', 1)[1].strip().split(' ', 2)[2]
        if group not in groups:
            groups.append(group)
        key = normalize(symbol)
        if key not in selected:
            continue
        rows.append('\t'.join((str(groups.index(group)), symbol, name, names.get(key, ''), keywords.get(key, ''))))
Path(__file__).with_name('emoji.tsv').write_text('# Unicode Emoji 17.0; CLDR 48 Russian annotations. License: UNICODE-3.0\n' + '\n'.join(rows) + '\n')
print(f'{len(rows)} emoji, {len(groups)} categories; {sum(bool(names.get(normalize(row.split(chr(9))[1]))) for row in rows)} Russian labels')
