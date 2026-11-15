<div align="center">

<img src="assets/brand/icon.svg" width="112" height="112" alt="TypeFlow icon">

# TypeFlow

**Smart language switcher for Windows**

Typed a word with the wrong keyboard layout? TypeFlow notices and fixes it with one key.<br>
English ⇄ Persian (فارسی).

[![Download TypeFlow for Windows](https://img.shields.io/badge/Download-TypeFlow.exe-2563EB?style=for-the-badge&logo=windows&logoColor=white)](../../releases/latest/download/TypeFlow.exe)

[All releases](../../releases) · [How to use](#how-to-use) · [Build from source](#build-from-source) · [Privacy](#privacy)

![Platform: Windows 10/11](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D4)
![Written in Rust](https://img.shields.io/badge/written%20in-Rust-B7410E)
![License: MIT](https://img.shields.io/badge/license-MIT-16A34A)
![Status: early](https://img.shields.io/badge/status-early-F59E0B)

</div>

---

## What it does

You meant to write **سلام** but the keyboard was on English, so you got `sghl`. TypeFlow watches for
this, shows a small bubble with the word you meant, and lets you convert it **and** switch the
layout with a single <kbd>Tab</kbd>.

| You type | Layout was | TypeFlow suggests |
|---|---|---|
| `sghl` | English | **سلام** |
| `ldo,hl` | English | **میخوام** |
| `sghl o,fd?` | English | **سلام خوبی؟** |
| `اثممخ` | Persian | **hello** |

Real words in the layout you're using are left alone.

### Highlights

- ⚡ **Instant**: checks after every Space, or after a short pause in search boxes.
- 🎯 **Accurate**: uses word-frequency lists for both languages and reads whole phrases, not just single words.
- ⌨️ **One key to fix**: <kbd>Tab</kbd> converts the text and switches the layout for you.
- 🤖 **Optional auto-fix**: converts on its own when it's sure.
- 📍 **Stays out of the way**: the bubble appears next to the text cursor without covering what you're typing.
- 🔒 **Private**: no network code, nothing saved to disk except your settings.
- 📦 **One small file**: a single `.exe` with no installer and nothing extra to install.

> [!NOTE]
> TypeFlow is in early development. It runs from the system tray and doesn't have an installer yet.

## Install

1. **[Download `TypeFlow.exe`](../../releases/latest/download/TypeFlow.exe)** from the latest release.
2. Make sure both the **English** and **Persian** keyboards are installed in Windows:
   *Settings → Time & language → Language & region*.
3. Run `TypeFlow.exe`. A blue **ف** icon appears next to the clock. That's it.

TypeFlow starts with Windows by default. You can turn that off from the tray menu.

> [!TIP]
> Windows SmartScreen may warn you because the app isn't code-signed yet. Click
> **More info → Run anyway**.

## How to use

Just type anywhere. When the last few words look like they were typed in the wrong layout, a bubble
appears near the text cursor with a short ding. If you keep typing in the wrong layout, the bubble
grows to cover the whole phrase.

### Keyboard and mouse

| Action | What it does |
|---|---|
| <kbd>Tab</kbd> (only while the bubble is showing) | Convert the text and switch the keyboard layout |
| **Click** the bubble | Same as <kbd>Tab</kbd> |
| <kbd>Esc</kbd>, or just ignore it | Dismiss (the bubble disappears after 5 seconds) |
| <kbd>Pause</kbd>, or tap <kbd>Right Ctrl</kbd> on its own | Accept the bubble; with no bubble, convert the last word |

### Tray icon

TypeFlow lives in the notification area as a rounded square with **ف**: blue when on, grey when off.
Only one copy can run at a time.

- **Left click**: turn hints on or off
- **Right click**: open the menu

| Menu item | Default | Description |
|---|---|---|
| Hints on | On | Show the bubble when a wrong layout is detected |
| Auto-fix when sure | Off | Convert without asking when TypeFlow is confident |
| Sound | On | Play a short ding when a bubble appears |
| Start with Windows | On | Launch TypeFlow when you sign in |
| Exit | | Quit TypeFlow |

<details>
<summary><b>Where the bubble appears</b></summary>

<br>

The bubble shows below the text cursor, on the side the text is running towards (to the right
while typing English letters, to the left while typing Persian ones), so it covers neither the line
you're typing nor what you already wrote.

- Near the bottom of the screen or the taskbar, it moves above the cursor.
- Near a side edge, it flips to the other side.
- It always stays at least 8 px inside the screen it's on.

The cursor position comes from the classic Win32 caret (Notepad, Word…) or from accessibility APIs
(Chrome, Edge, Firefox, Electron apps). Only if neither works does it fall back to the mouse pointer.

</details>

<details>
<summary><b>Settings file and advanced options</b></summary>

<br>

Settings are saved to `%APPDATA%\TypeFlow\settings.ini` whenever you change them. You can also
edit that file by hand:

```ini
enabled=true
auto_fix=false
sound=true
start_with_windows=true
```

**Start with Windows** adds `TypeFlow` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, pointing at the exe you ran. If you move the
exe, start it once from the new place and the entry follows. Debug builds never touch that entry.

More options are constants at the top of [`src/win.rs`](src/win.rs): `SWITCH_LAYOUT`,
`HOTKEY_VK`, `RCTRL_TAP`, `TAB_ACCEPTS`, `IDLE_MS` and `BUBBLE_MS`.

**Upgrading from Smart Keyboard or layout-hint:** TypeFlow was previously called *Smart Keyboard*
and before that *layout-hint*. On first start it takes over settings saved under the old names and
replaces the old Start with Windows entry. You can delete the old exe.

</details>

## Privacy

Keys are kept **only in memory**, only for the words currently being checked, and are cleared on a
click, a window switch or <kbd>Enter</kbd>. Nothing you type is written to disk or sent anywhere.
TypeFlow has no network code at all.

## Build from source

You need [Rust](https://rustup.rs/). With the default MSVC toolchain you also need the
[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
(*Desktop development with C++*). Without them, use the GNU toolchain:

```bat
rustup toolchain install stable-x86_64-pc-windows-gnu
cargo +stable-x86_64-pc-windows-gnu test
cargo +stable-x86_64-pc-windows-gnu run --release
```

The app is built to `target\release\typeflow.exe`, a single file with everything embedded.

- The release build has no console window; quit it with **Exit** in the tray menu.
- A debug build (`cargo run` without `--release`) keeps the console and logs each `hint: ...` there.
- In Git Bash, Git's own `link` command can shadow the MSVC linker. Build from `cmd` or PowerShell.
- To preview the bubble and tray icon without running the app:
  `cargo test previews -- --ignored`, then open `target/preview/*.bmp`.

The word lists in `data/` are committed. To regenerate them:

```bat
pip install wordfreq==3.1.1
python tools/make_wordlists.py
```

### Publishing a release

Push a version tag and GitHub Actions builds `TypeFlow.exe` and attaches it to a new release (see
[`.github/workflows/release.yml`](.github/workflows/release.yml)):

```bash
git tag v0.1.0
git push origin v0.1.0
```

## How it works

| File | Role |
|---|---|
| [`src/win.rs`](src/win.rs) | A low-level keyboard hook records each key together with what it produces in **your own** installed English and Persian layouts (`ToUnicodeEx`), so both readings of every word are known. The bubble is a plain Win32 window drawn with GDI, which shapes and orders Persian text correctly. Conversion is Backspace × n plus `SendInput` (Unicode), followed by `WM_INPUTLANGCHANGEREQUEST` to switch the layout. |
| [`src/detect.rs`](src/detect.rs) | Looks both readings up in word-frequency lists (Zipf scale). If neither is a known word, it falls back to a character-bigram model and only suggests. It scores the last 5 words together, so short or ambiguous words (`تو`, `رو`, `,` → `و`) inside a sentence typed in the wrong layout add up instead of breaking the hint, while a word that is clearly right (or a number) ends the phrase. |
| [`src/settings.rs`](src/settings.rs) | The settings file and the Start with Windows entry. |
| [`src/tray.rs`](src/tray.rs) | The tray icon, drawn at runtime (the "ف" is measured and fitted to the icon size), so there is no `.ico` file or resource compiler step. |
| [`src/mapping.rs`](src/mapping.rs) | A static key map used as a fallback and by the tests. |

The bubble uses **Vazirmatn RD Light** for Persian and **Ubuntu Sans Condensed Light** for English.
Both fonts are embedded in the exe and registered only for this process, so nothing is installed on
your system. The ding is `assets/sounds/notification.wav`, also embedded and played with
`PlaySound`.

## Acknowledgements

TypeFlow is a Rust port of **[ZabanYar](https://github.com/shyguy2018/ZabanYar)** by
**Behrooz Shayestepoor** (MIT). The detection approach and thresholds, the fallback key map, the
test cases and the overall design of the key tracker come from ZabanYar. Thank you!

- Word frequencies: **[wordfreq](https://github.com/rspeer/wordfreq)** by Robyn Speer (CC BY-SA 4.0)
- Fonts: **[Vazirmatn](https://github.com/rastikerdar/vazirmatn)** by Saber Rastikerdar and the
  Vazirmatn Project Authors (OFL 1.1), and
  **[Ubuntu Sans](https://github.com/canonical/Ubuntu-Sans-fonts)** by Canonical (Ubuntu Font Licence 1.0)
- Notification sound: "New Notification 07" by **Universfield** (Pixabay Content License)

## License

| Part | License |
|---|---|
| Code | [MIT](LICENSE) |
| Word lists in `data/` | [CC BY-SA 4.0](data/SOURCES.md) |
| Fonts in `assets/fonts/` | Ubuntu Font Licence 1.0 and SIL OFL 1.1 (license texts next to the fonts) |
| Icon in `assets/brand/` | Contains the "ف" glyph from Vazirmatn (SIL OFL 1.1) |
| Third-party notices | [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) |
