# Third-party notices

## ZabanYar

TypeFlow is a Rust port of parts of [ZabanYar](https://github.com/shyguy2018/ZabanYar)
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

The Persian font is embedded in the exe and registered privately for the app only
(`AddFontMemResourceEx`); nothing is installed on the system.

| File | Font | Source | License |
|---|---|---|---|
| `assets/fonts/Vazirmatn-RD-Light.ttf` | Vazirmatn RD (Round Dots) Light, © 2015 The Vazirmatn Project Authors | [rastikerdar/vazirmatn](https://github.com/rastikerdar/vazirmatn) | SIL Open Font License 1.1, see [assets/fonts/Vazirmatn-OFL.txt](assets/fonts/Vazirmatn-OFL.txt) |

The font file is unmodified. English text uses Segoe UI, which ships with Windows and is not bundled.

## Notification sound

`assets/sounds/notification.wav` is "Notification Alert Warning" from
**[Tunetank](https://tunetank.com)** (file `tunetank.com_notification-alert-warning.wav`, © 2023),
used under the Tunetank license (TODO: confirm the license terms and any attribution they ask for).
Changes: trimmed to 0–1.81 s with a 0.1 s fade-out, mixed down to 16-bit 44.1 kHz mono WAV, tags
removed.

## windows-rs

The [`windows`](https://github.com/microsoft/windows-rs) crate by Microsoft is used under the
MIT or Apache-2.0 license.
