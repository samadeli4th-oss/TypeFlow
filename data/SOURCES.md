# Word lists

`en.txt` and `fa.txt` contain one `word<TAB>Zipf frequency` per line.

- **Source:** [wordfreq](https://github.com/rspeer/wordfreq) 3.1.1 by Robyn Speer
- **License:** [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). These files are
  adapted from wordfreq's data, so they are licensed under CC BY-SA 4.0 too. The rest of the
  project is MIT.
- **Changes:** kept the 70,000 most frequent English words matching `[a-z]+('[a-z]+)?` and the
  90,000 most frequent Persian entries (about 30,000 exist) made only of Arabic-script letters and
  the zero-width non-joiner; frequencies rounded to 2 decimals.
- **How:** `tools/make_wordlists.py`, adapted from
  [ZabanYar](https://github.com/shyguy2018/ZabanYar)'s script of the same name.
