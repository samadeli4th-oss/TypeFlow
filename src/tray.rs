//! Notification-area (tray) icon, its menu, and the runtime settings it toggles.
//!
//! Left click: turn hints on/off. Right click: menu.
//! The icon is the TypeFlow artwork from assets/icons/: in color when hints
//! are on, grey when they're off.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::Settings;

/// Message the shell sends us for clicks on the icon.
pub const WM_TRAY: u32 = WM_APP + 20;
const ICON_ID: u32 = 1;

pub const CMD_ENABLED: usize = 1;
pub const CMD_AUTO_FIX: usize = 2;
pub const CMD_DING: usize = 3;
pub const CMD_AUTOSTART: usize = 4;
pub const CMD_EXIT: usize = 9;

/// Hints on/off.
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// Convert automatically when sure, instead of only showing a hint.
pub static AUTO_FIX: AtomicBool = AtomicBool::new(false);
/// Short ding when a new bubble appears.
pub static DING: AtomicBool = AtomicBool::new(true);
/// Run at Windows sign-in.
pub static START_WITH_WINDOWS: AtomicBool = AtomicBool::new(true);

pub fn get(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

pub fn apply(s: &Settings) {
    ENABLED.store(s.enabled, Ordering::Relaxed);
    AUTO_FIX.store(s.auto_fix, Ordering::Relaxed);
    DING.store(s.sound, Ordering::Relaxed);
    START_WITH_WINDOWS.store(s.start_with_windows, Ordering::Relaxed);
}

pub fn current() -> Settings {
    Settings {
        enabled: get(&ENABLED),
        auto_fix: get(&AUTO_FIX),
        sound: get(&DING),
        start_with_windows: get(&START_WITH_WINDOWS),
    }
}

pub fn toggle(flag: &AtomicBool) -> bool {
    !flag.fetch_xor(true, Ordering::Relaxed)
}

thread_local! {
    static CURRENT_ICON: Cell<isize> = const { Cell::new(0) };
}

fn notify_data(window: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    let tip = if get(&ENABLED) { "TypeFlow: on" } else { "TypeFlow: off" };
    for (i, unit) in tip.encode_utf16().take(data.szTip.len() - 1).enumerate() {
        data.szTip[i] = unit;
    }
    data
}

fn set_icon(window: HWND, message: NOTIFY_ICON_MESSAGE) {
    let icon = make_icon(get(&ENABLED));
    unsafe {
        let _ = Shell_NotifyIconW(message, &notify_data(window, icon));
        let old = CURRENT_ICON.with(|c| c.replace(icon.0 as isize));
        if old != 0 {
            let _ = DestroyIcon(HICON(old as _));
        }
    }
}

/// Adds the icon (also call again when Explorer restarts: "TaskbarCreated").
pub fn add(window: HWND) {
    set_icon(window, NIM_ADD);
}

/// Redraws the icon and tooltip after a setting changed.
pub fn update(window: HWND) {
    set_icon(window, NIM_MODIFY);
}

pub fn remove(window: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: ICON_ID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// Shows the right-click menu at the mouse and returns the chosen command (0 = none).
pub fn show_menu(window: HWND) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        let item = |flags: MENU_ITEM_FLAGS, id: usize, text: PCWSTR| {
            let _ = AppendMenuW(menu, flags, id, text);
        };
        let check = |flag: &AtomicBool| if get(flag) { MF_CHECKED } else { MF_UNCHECKED };
        item(MF_STRING | MF_GRAYED, 0, w!("TypeFlow  ·  English ⇄ فارسی"));
        item(MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING | check(&ENABLED), CMD_ENABLED, w!("Hints on"));
        item(MF_STRING | check(&AUTO_FIX), CMD_AUTO_FIX, w!("Auto-fix when sure"));
        item(MF_STRING | check(&DING), CMD_DING, w!("Sound"));
        item(MF_STRING | check(&START_WITH_WINDOWS), CMD_AUTOSTART, w!("Start with Windows"));
        item(MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING, CMD_EXIT, w!("Exit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // needed so the menu closes when the user clicks elsewhere
        let _ = SetForegroundWindow(window);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, 0, window, None);
        let _ = PostMessageW(window, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    }
}

// --------------------------------------------------------------------- icon ---

/// The TypeFlow tray artwork (assets/icons/, made by tools/make_icons.py):
/// in color while hints are on, grey while they're off.
static TRAY_ON: &[u8] = include_bytes!("../assets/icons/tray.ico");
static TRAY_OFF: &[u8] = include_bytes!("../assets/icons/tray-off.ico");

/// The image in an .ico file best suited to `size` px: the smallest one at
/// least that big, else the biggest. Returns its bytes.
pub fn ico_image(ico: &[u8], size: i32) -> Option<&[u8]> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]) as usize;
    if ico.len() < 6 || u16_at(2) != 1 {
        return None;
    }
    let entries = (0..u16_at(4)).map(|i| 6 + i * 16).filter(|e| e + 16 <= ico.len());
    // width 0 means 256
    let side = |e: usize| if ico[e] == 0 { 256 } else { ico[e] as i32 };
    let best = entries.min_by_key(|&e| if side(e) >= size { side(e) - size } else { 1000 + size - side(e) })?;
    ico.get(u32_at(best + 12)..u32_at(best + 12) + u32_at(best + 8))
}

fn make_icon(enabled: bool) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let Some(image) = ico_image(if enabled { TRAY_ON } else { TRAY_OFF }, size) else { return HICON::default() };
    unsafe { CreateIconFromResourceEx(image, TRUE, 0x0003_0000, size, size, LR_DEFAULTCOLOR) }.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_closest_tray_image() {
        for (size, want) in [(16, 16), (18, 20), (28, 28), (36, 40), (100, 64)] {
            let image = ico_image(TRAY_ON, size).unwrap();
            // PNG entries carry their width at byte 16 of the IHDR chunk
            let w = if image.starts_with(b"\x89PNG") {
                u32::from_be_bytes([image[16], image[17], image[18], image[19]]) as i32
            } else {
                i32::from_le_bytes([image[4], image[5], image[6], image[7]])
            };
            assert_eq!(w, want, "for {size} px");
        }
        assert!(ico_image(TRAY_OFF, 16).is_some());
    }
}
