//! layout-hint: shows a hint when a word was typed with the wrong keyboard
//! layout (English <-> Persian) on Windows.
//!
//! Based on ZabanYar by Behrooz Shayestepoor (https://github.com/shyguy2018/ZabanYar, MIT).
//! See README.md and THIRD_PARTY_LICENSES.md.

// Release builds are tray-only (no console window); debug builds keep the
// console so `cargo run` shows the `hint: ...` log.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod detect;
mod mapping;
mod settings;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    {
        let started = std::time::Instant::now();
        detect::lexicons(); // build the dictionaries before the hook starts
        println!("layout-hint: word lists loaded in {:?}", started.elapsed());
        println!("Type anywhere. Tab = accept hint, Esc = dismiss, Pause or Right Ctrl tap = convert last word.");
        println!("Tray icon: left click = on/off, right click = menu (Exit).");
        if let Err(e) = win::run() {
            let text: Vec<u16> = format!("layout-hint could not start:\n{e}\0").encode_utf16().collect();
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    None,
                    windows::core::PCWSTR(text.as_ptr()),
                    windows::core::w!("layout-hint"),
                    windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    eprintln!("layout-hint only runs on Windows.");
}
