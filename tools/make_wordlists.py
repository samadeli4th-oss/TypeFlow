"""Regenerates data/en.txt and data/fa.txt (word<TAB>Zipf frequency).

Adapted from ZabanYar's tools/make_wordlists.py
(https://github.com/shyguy2018/ZabanYar, MIT, (c) 2026 Behrooz Shayestepoor).
The word data comes from wordfreq by Robyn Speer (CC BY-SA 4.0), so the
generated files are CC BY-SA 4.0 too. See data/SOURCES.md.

Usage:  pip install wordfreq==3.1.1  &&  python tools/make_wordlists.py
"""
import os
import re

import wordfreq

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'data')
os.makedirs(ROOT, exist_ok=True)

en = [w for w in wordfreq.top_n_list('en', 70000) if re.fullmatch(r"[a-z]+('[a-z]+)?", w)]
fa = [w for w in wordfreq.top_n_list('fa', 90000) if re.fullmatch('[؀-ۿ‌]+', w)]
with open(os.path.join(ROOT, 'en.txt'), 'w', encoding='utf-8', newline='\n') as f:
    f.write('\n'.join(f'{w}\t{wordfreq.zipf_frequency(w, "en"):.2f}' for w in en))
with open(os.path.join(ROOT, 'fa.txt'), 'w', encoding='utf-8', newline='\n') as f:
    f.write('\n'.join(f'{w}\t{wordfreq.zipf_frequency(w, "fa"):.2f}' for w in fa))
print(len(en), 'English words,', len(fa), 'Persian words')
