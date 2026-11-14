//! Settings saved between runs, and the "Start with Windows" registry entry.
//!
//! File: `%APPDATA%\TypeFlow\settings.ini`, plain `key=value` lines:
//!
//! ```text
//! enabled=true
//! auto_fix=false
//! sound=true
//! start_with_windows=true
//! ```
//!
//! Unknown keys and bad values are ignored (the default is used), so the file
//! can be edited by hand.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub auto_fix: bool,
    pub sound: bool,
    pub start_with_windows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, auto_fix: false, sound: true, start_with_windows: true }
    }
}

impl Settings {
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = match value.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => true,
                "false" | "0" | "no" | "off" => false,
                _ => continue,
            };
            match key.trim() {
                "enabled" => s.enabled = value,
                "auto_fix" => s.auto_fix = value,
                "sound" => s.sound = value,
                "start_with_windows" => s.start_with_windows = value,
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        format!(
            "# TypeFlow settings (also changed from the tray menu)\n\
             enabled={}\nauto_fix={}\nsound={}\nstart_with_windows={}\n",
            self.enabled, self.auto_fix, self.sound, self.start_with_windows
        )
    }
}

fn settings_file(app_folder: &str) -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join(app_folder).join("settings.ini"))
}

fn path() -> Option<PathBuf> {
    settings_file("TypeFlow")
}

/// Folders the app saved settings in under its earlier names, newest first.
const OLD_FOLDERS: [&str; 2] = ["Smart Keyboard", "layout-hint"];

/// Loads the saved settings. With no file yet, takes over the settings saved
/// under one of the app's old names, else uses the defaults.
pub fn load() -> Settings {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .or_else(|| OLD_FOLDERS.iter().find_map(|f| settings_file(f).and_then(|p| std::fs::read_to_string(p).ok())))
        .map(|text| Settings::parse(&text))
        .unwrap_or_default()
}

pub fn save(settings: &Settings) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, settings.to_text()) {
        eprintln!("could not save {}: {e}", p.display());
    }
}

// ------------------------------------------------------- start with Windows ---
#[cfg(windows)]
pub mod autostart {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Registry::*;

    const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const VALUE: PCWSTR = w!("TypeFlow");
    /// Entries written under the app's old names.
    const OLD_VALUES: [PCWSTR; 2] = [w!("Smart Keyboard"), w!("layout-hint")];

    /// Adds or removes `HKCU\...\Run\TypeFlow`. When adding, it points at the
    /// exe that is running now, so moving the exe and starting it once fixes
    /// the entry. Any entry from an old name is removed.
    pub fn set(on: bool) {
        unsafe {
            for old in OLD_VALUES {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, old);
            }
            if on {
                let Ok(exe) = std::env::current_exe() else { return };
                let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
                let _ = RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    VALUE,
                    REG_SZ.0,
                    Some(command.as_ptr() as *const _),
                    (command.len() * 2) as u32,
                );
            } else {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert!(Settings::default().start_with_windows);
    }

    #[test]
    fn round_trip() {
        let s = Settings { enabled: false, auto_fix: true, sound: false, start_with_windows: false };
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn tolerates_hand_edits() {
        let s = Settings::parse("# comment\n sound = off \nauto_fix=maybe\nunknown=true\nenabled=0\n");
        assert_eq!(s, Settings { enabled: false, sound: false, ..Settings::default() });
    }
}
