# Third-party notices

## ZabanYar

layout-hint is a Rust port of parts of [ZabanYar](https://github.com/shyguy2018/ZabanYar)
by Behrooz Shayestepoor:

- `src/detect.rs`: detection logic, thresholds and test cases (from `core.py`, `tests/test_core.py`)
- `src/mapping.rs`: fallback key-mapping table (from `core.py`)
- `src/win.rs`: overall design of the key tracker, conversion and idle check (from `app.py`)
- `tools/make_wordlists.py`: word-list generator (from `tools/make_wordlists.py`)

ZabanYar is distributed under the MIT License:

```
MIT License

Copyright (c) 2026 Behrooz Shayestepoor

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## wordfreq data

`data/en.txt` and `data/fa.txt` are generated from [wordfreq](https://github.com/rspeer/wordfreq)
by Robyn Speer. wordfreq's data is licensed under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/); see [data/SOURCES.md](data/SOURCES.md)
for what was changed.

## Fonts

Both fonts are embedded in the exe and registered privately for the app only
(`AddFontMemResourceEx`); nothing is installed on the system.

| File | Font | Source | License |
|---|---|---|---|
| `assets/fonts/UbuntuSansCondensed-Light.ttf` | Ubuntu Sans Condensed Light, © 2011, 2022, 2023 Canonical Ltd. | [canonical/Ubuntu-Sans-fonts](https://github.com/canonical/Ubuntu-Sans-fonts) | Ubuntu Font Licence 1.0, see [assets/fonts/UbuntuSans-LICENCE.txt](assets/fonts/UbuntuSans-LICENCE.txt) |
| `assets/fonts/Vazirmatn-RD-Light.ttf` | Vazirmatn RD (Round Dots) Light, © 2015 The Vazirmatn Project Authors | [rastikerdar/vazirmatn](https://github.com/rastikerdar/vazirmatn) | SIL Open Font License 1.1, see [assets/fonts/Vazirmatn-OFL.txt](assets/fonts/Vazirmatn-OFL.txt) |

The font files are unmodified.

## Notification sound

`assets/sounds/notification.wav` is "New Notification 07" by **Universfield**
(file `universfield-new-notification-07-210334.mp3`, as distributed on
[Pixabay](https://pixabay.com/users/universfield-28281460/)), used under the
[Pixabay Content License](https://pixabay.com/service/license-summary/).
Changes: trimmed to 0.095–0.95 s with a short fade-out, converted to 16-bit
44.1 kHz mono WAV.

## windows-rs

The [`windows`](https://github.com/microsoft/windows-rs) crate by Microsoft is used under the
MIT or Apache-2.0 license.
