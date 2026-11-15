# TypeFlow, Smart language switcher for subdued

Shows a small hint when you type a word with the wrong keyboard layout, English ⇄ Persian, on Windows.

| You type | Layout was | Hint |
|---|---|---|
| `sghl` | English | **سلام** |
| `ldo,hl` | English | **میخوام** |
| `اثممخ` | Persian | **hello** |

Real words in the layout you're using are left alone.

> **Status:** early. It runs from the tray and has no installer yet.

## Use

Both the **English** and **Persian** keyboards must be installed in Windows
(Settings → Time & language → Language & region).

Run it, then type anywhere. After each Space, or after a short pause (for search boxes), the last
few words are checked together. If they look like the wrong layout, a bubble appears near the text
caret with a short ding. If you keep typing in the wrong layout, the bubble grows to cover the
whole phrase, for example `sghl o,fd?` → **سلام خوبی؟**.

**Where the bubble appears:** below the text caret, on the side the text is running towards (to
the right while typing English letters, to the left while typing Persian ones), so it covers
neither the line you're typing nor what you already wrote. Near the bottom of the screen or the
taskbar it moves above the caret; near a side edge it flips to the other side; it always stays at
least 8 px inside the screen it's on. The caret is found through the classic Win32 caret (Notepad,
Word…) or through accessibility (Chrome, Edge, Firefox, Electron apps); only if neither works does
it fall back to the mouse pointer.

| Key | What it does |
|---|---|
| **Tab** (only while the bubble is showing) | Convert the text and switch the keyboard layout |
| **Click** the bubble | Same as Tab |
| **Esc**, or ignore it | Dismiss (it disappears after 5 seconds) |
| **Pause**, or **tap Right Ctrl** on its own | Accept the bubble; with no bubble, convert the last word |

### Tray icon

The app lives in the notification area as a rounded square with **ف**: blue when on, grey when off.
Only one copy can run at a time.

- **Left click**: turn hints on/off
- **Right click**: menu with **Hints on**, **Auto-fix when sure** (convert without asking; off by
  default), **Sound** (the ding), **Start with Windows** (on by default), and **Exit**

Settings are saved to `%APPDATA%\Smart Keyboard\settings.ini` whenever you change them, and you can
also edit that file by hand. **Start with Windows** adds `Smart Keyboard` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, pointing at the exe you ran. If you move the
exe, start it once from the new place and the entry follows. Debug builds never touch that entry.

The release build has no console window, so quit it with **Exit** in the tray menu. A debug build
(`cargo run` without `--release`) keeps the console and logs each `hint: ...` there.

Other settings are constants at the top of [`src/win.rs`](src/win.rs): `SWITCH_LAYOUT`,
`HOTKEY_VK`, `RCTRL_TAP`, `TAB_ACCEPTS`, `IDLE_MS`, `BUBBLE_MS`.

## Build

Needs Rust. With the default MSVC toolchain you also need the
[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) ("Desktop
development with C++"). Without them, use the GNU toolchain:

```bat
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu test
cargo +stable-x86_64-pc-windows-gnu run --release
```

The app is `target\release\smart-keyboard.exe`, a single file with everything built in.

Smart Keyboard was first called *layout-hint*. On first start it takes over settings saved under
`%APPDATA%\layout-hint\` and replaces the old `layout-hint` Start with Windows entry.

In Git Bash, Git's own `link` command can shadow the MSVC linker; build from `cmd` or PowerShell.

The word lists in `data/` are committed. To regenerate them:

```bat
pip install wordfreq==3.1.1
python tools/make_wordlists.py
```

## How it works

- [`src/win.rs`](src/win.rs): a low-level keyboard hook records each key together with what it
  produces in **your own** installed English and Persian layouts (`ToUnicodeEx`), so both readings
  of every word are known. The bubble is a plain Win32 window drawn with GDI, which shapes and
  orders Persian text correctly. Conversion is Backspace × n plus `SendInput` (Unicode), followed
  by `WM_INPUTLANGCHANGEREQUEST` to switch the layout.
- [`src/detect.rs`](src/detect.rs): looks both readings up in word-frequency lists (Zipf scale). If
  neither is a known word, it falls back to a character-bigram model and only suggests. It also
  scores the last 5 words together, so short or ambiguous words (`تو`, `رو`, `,` → `و`) inside a
  sentence typed in the wrong layout add up instead of breaking the hint, while a word that is
  clearly right (or a number) ends the phrase.
- The ding is `assets/sounds/notification.wav`, built into the exe and played with `PlaySound`.
- [`src/settings.rs`](src/settings.rs): the settings file and the Start with Windows entry.
- [`src/tray.rs`](src/tray.rs): the tray icon is drawn at runtime (the "ف" is measured and fitted
  to the icon size), so there is no `.ico` file or resource compiler step.
- Fonts: the bubble uses **Vazirmatn RD Light** for Persian and **Ubuntu Sans Condensed Light** for
  English. Both are embedded in the exe and registered only for this process, so nothing is
  installed on your system. To see the bubble and icon without running the app:
  `cargo test previews -- --ignored`, then open `target/preview/*.bmp`.
- [`src/mapping.rs`](src/mapping.rs): a static key map used as a fallback and by the tests.

**Privacy:** keys are kept only in memory, only for the current run of words, and are cleared on a
click, a window switch or Enter. Nothing is written to disk or sent anywhere; there is no network
code.

## Acknowledgements

Smart Keyboard is a Rust port of **[ZabanYar](https://github.com/shyguy2018/ZabanYar)** by
**Behrooz Shayestepoor** (MIT). The detection approach and thresholds, the fallback key map, the
test cases and the overall design of the key tracker come from ZabanYar. Thank you!

Word frequencies come from **[wordfreq](https://github.com/rspeer/wordfreq)** by Robyn Speer
(CC BY-SA 4.0).

Fonts: **[Vazirmatn](https://github.com/rastikerdar/vazirmatn)** by Saber Rastikerdar and the
Vazirmatn Project Authors (OFL 1.1), and **[Ubuntu Sans](https://github.com/canonical/Ubuntu-Sans-fonts)**
by Canonical (Ubuntu Font Licence 1.0).

Notification sound: "New Notification 07" by **Universfield** (Pixabay Content License).

## License

- Code: [MIT](LICENSE)
- Word lists in `data/`: [CC BY-SA 4.0](data/SOURCES.md)
- Fonts in `assets/fonts/`: Ubuntu Font Licence 1.0 and SIL OFL 1.1 (license texts next to the fonts)
- Third-party notices: [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)
