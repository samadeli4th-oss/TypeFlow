# TypeFlow

Shows a small hint when you type a word with the wrong keyboard layout, English ⇄ Persian, on Windows.

| You type | Layout was | Hint |
|---|---|---|
| `sghl` | English | **سلام** |
| `ldo,hl` | English | **میخوام** |
| `اثممخ` | Persian | **hello** |

Real words in the layout you're using are left alone.

> **Status:** early. It runs from the tray.

## Install

Run `TypeFlow-Setup-<version>.exe`. It installs TypeFlow for your user only, so no admin rights
are needed:

- the app goes to `%LOCALAPPDATA%\Programs\TypeFlow\`, with a Start menu shortcut;
- if the English or Persian keyboard is missing, it offers to add it (the same as adding the
  language in Settings);
- it starts TypeFlow, which adds itself to Start with Windows;
- an update closes the running copy first and keeps your settings.

The exe isn't signed, so Windows SmartScreen may warn about an unknown publisher: **More info** →
**Run anyway**. To remove TypeFlow, use Settings → Apps → Installed apps → TypeFlow → Uninstall;
your settings in `%APPDATA%\TypeFlow\` are kept.

For scripted installs, `TypeFlow-Setup-<version>.exe /S` installs without questions (and leaves
the keyboards alone), and `%LOCALAPPDATA%\Programs\TypeFlow\uninstall.exe /uninstall /S` removes it.

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

The app lives in the notification area as **A ⇄** over **س**: in color when on, grey when off.
Only one copy can run at a time.

- **Left click**: turn hints on/off
- **Right click**: menu with **Hints on**, **Auto-fix when sure** (convert without asking; off by
  default), **Sound** (the ding), **Start with Windows** (on by default), and **Exit**

Settings are saved to `%APPDATA%\TypeFlow\settings.ini` whenever you change them, and you can
also edit that file by hand. **Start with Windows** adds `TypeFlow` under
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

The app is `target\release\typeflow.exe` (or `target\gnu\release\typeflow.exe` with the GNU
toolchain), a single file with everything built in.

### Installer

```bat
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1 -Gnu
```

(the second line uses the GNU toolchain). This builds the release app and then
[`installer/`](installer/), and writes `dist\TypeFlow-Setup-<version>.exe`: one file with the app
inside, nothing else to download. See [Install](#install) for what it does.

TypeFlow was earlier called *Smart Keyboard* and, before that, *layout-hint*. On first start it
takes over settings saved under `%APPDATA%\Smart Keyboard\` (or `%APPDATA%\layout-hint\`) and
replaces the old Start with Windows entries.

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
- Icons: `assets/icons/typeflow.ico` is the app icon and `tray.ico` / `tray-off.ico` the tray
  icon. All three are cut from the artwork sheet by `tools/make_icons.py` (needs Pillow and NumPy).
  [`build.rs`](build.rs) puts the app icon into the exe by writing the resource object itself, so
  no resource compiler is needed; [`src/tray.rs`](src/tray.rs) loads the tray icon at the size
  Windows asks for.
- [`src/theme.rs`](src/theme.rs): the bubble is a small acrylic glass pill that shows only the
  suggestion. It follows Settings › Personalization › Colors: light or dark mode, the accent color
  as a tint when "Show accent color on Start and taskbar" is on, and a solid fill when
  Transparency effects are off. The settings are re-read every time a bubble appears.
- Fonts: the bubble uses **Vazirmatn RD Light** for Persian, embedded in the exe and registered
  only for this process, and the Windows 11 system font **Segoe UI Variable Text Semilight** for
  English (Segoe UI Semilight on Windows 10). To see the bubble without running the app:
  `cargo test previews -- --ignored`, then open `target/preview/*.bmp`.
  To see the real glass, run a debug build with `SMART_KEYBOARD_DEMO` set to some text
  (e.g. `سلام`): it shows that hint at startup.
- [`src/mapping.rs`](src/mapping.rs): a static key map used as a fallback and by the tests.

**Privacy:** keys are kept only in memory, only for the current run of words, and are cleared on a
click, a window switch or Enter. Nothing is written to disk or sent anywhere; there is no network
code.

## Acknowledgements

TypeFlow is a Rust port of **[ZabanYar](https://github.com/shyguy2018/ZabanYar)** by
**Behrooz Shayestepoor** (MIT). The detection approach and thresholds, the fallback key map, the
test cases and the overall design of the key tracker come from ZabanYar. Thank you!

Word frequencies come from **[wordfreq](https://github.com/rspeer/wordfreq)** by Robyn Speer
(CC BY-SA 4.0).

Fonts: **[Vazirmatn](https://github.com/rastikerdar/vazirmatn)** by Saber Rastikerdar and the
Vazirmatn Project Authors (OFL 1.1).

Notification sound: "Notification Alert Warning" from **[Tunetank](https://tunetank.com)**.

## License

- Code: [MIT](LICENSE)
- Word lists in `data/`: [CC BY-SA 4.0](data/SOURCES.md)
- Font in `assets/fonts/`: SIL OFL 1.1 (license text next to the font)
- Third-party notices: [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)
