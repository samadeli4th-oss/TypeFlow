//! Notification-area (tray) icon, its menu, and the runtime settings it toggles.
//!
//! Left click: open the TypeFlow window (panel.rs). Right click: menu.
//! The icon is the TypeFlow artwork from assets/icons/: in color when hints
//! are on, grey when they're off.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::{FieldAction, Settings};

/// Message the shell sends us for clicks on the icon.
pub const WM_TRAY: u32 = WM_APP + 20;
/// The TypeFlow window asks for a command (wparam = CMD_*).
pub const WM_PANEL_COMMAND: u32 = WM_APP + 21;
const ICON_ID: u32 = 1;

pub const CMD_ENABLED: usize = 1;
pub const CMD_AUTO_FIX: usize = 2;
pub const CMD_DING: usize = 3;
pub const CMD_AUTOSTART: usize = 4;
pub const CMD_AUTOCORRECT_EN: usize = 5;
pub const CMD_AUTOCORRECT_FA: usize = 6;
pub const CMD_OPEN: usize = 7;
pub const CMD_EXIT: usize = 9;
pub const CMD_FIELD_ENGLISH: usize = 10;
pub const CMD_FIELD_NOTIFY: usize = 11;
pub const CMD_FIELD_DISABLE: usize = 12;
pub const CMD_LEARN: usize = 13;
pub const CMD_WORDS: usize = 14;

/// Hints on/off.
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// Convert automatically when sure, instead of only showing a hint.
pub static AUTO_FIX: AtomicBool = AtomicBool::new(false);
/// Short ding when a new bubble appears.
pub static DING: AtomicBool = AtomicBool::new(true);
/// Run at Windows sign-in.
pub static START_WITH_WINDOWS: AtomicBool = AtomicBool::new(true);
/// Auto-correct for English text.
pub static AUTOCORRECT_EN: AtomicBool = AtomicBool::new(false);
/// Auto-correct for Persian text.
pub static AUTOCORRECT_FA: AtomicBool = AtomicBool::new(false);
/// Learn a word once its hint or auto-fix was rejected 4 times.
pub static LEARN_WORDS: AtomicBool = AtomicBool::new(false);
/// Password and number fields (a `FieldAction`).
static FIELD_ACTION: AtomicU8 = AtomicU8::new(FieldAction::English as u8);

pub fn field_action() -> FieldAction {
    match FIELD_ACTION.load(Ordering::Relaxed) {
        1 => FieldAction::Notify,
        2 => FieldAction::Disable,
        _ => FieldAction::English,
    }
}

pub fn set_field_action(action: FieldAction) {
    FIELD_ACTION.store(action as u8, Ordering::Relaxed);
}

pub fn get(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

/// True while anything needs the keyboard: layout hints or auto-correct.
pub fn active() -> bool {
    get(&ENABLED) || get(&AUTOCORRECT_EN) || get(&AUTOCORRECT_FA)
}

pub fn apply(s: &Settings) {
    ENABLED.store(s.enabled, Ordering::Relaxed);
    AUTO_FIX.store(s.auto_fix, Ordering::Relaxed);
    DING.store(s.sound, Ordering::Relaxed);
    START_WITH_WINDOWS.store(s.start_with_windows, Ordering::Relaxed);
    AUTOCORRECT_EN.store(s.autocorrect_en, Ordering::Relaxed);
    AUTOCORRECT_FA.store(s.autocorrect_fa, Ordering::Relaxed);
    LEARN_WORDS.store(s.learn_words, Ordering::Relaxed);
    set_field_action(s.field_action);
}

pub fn current() -> Settings {
    Settings {
        enabled: get(&ENABLED),
        auto_fix: get(&AUTO_FIX),
        sound: get(&DING),
        start_with_windows: get(&START_WITH_WINDOWS),
        autocorrect_en: get(&AUTOCORRECT_EN),
        autocorrect_fa: get(&AUTOCORRECT_FA),
        field_action: field_action(),
        learn_words: get(&LEARN_WORDS),
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

/// Where the icon is on screen. Fails if the shell can't say.
pub fn icon_rect(window: HWND) -> Option<RECT> {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: window,
        uID: ICON_ID,
        ..Default::default()
    };
    unsafe { Shell_NotifyIconGetRect(&id) }.ok()
}

/// Shows the right-click menu at the mouse and returns the chosen command (0 = none).
pub fn show_menu(window: HWND) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        let item = |flags: MENU_ITEM_FLAGS, id: usize, text: PCWSTR| {
            let _ = AppendMenuW(menu, flags, id, text);
        };
        let check = |flag: &AtomicBool| if get(flag) { MF_CHECKED } else { MF_UNCHECKED };
        item(MF_STRING, CMD_OPEN, w!("Open TypeFlow"));
        let _ = SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);
        item(MF_SEPARATOR, 0, PCWSTR::null());
        item(MF_STRING | check(&ENABLED), CMD_ENABLED, w!("Hints on"));
        item(MF_STRING | check(&AUTO_FIX), CMD_AUTO_FIX, w!("Auto-fix when sure"));
        item(MF_STRING | check(&LEARN_WORDS), CMD_LEARN, w!("Learn new words"));
        item(MF_STRING, CMD_WORDS, w!("Add word…"));
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
    icon_of_size(enabled, unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16))
}

/// The tray artwork at `size` px. The caller destroys it.
pub fn icon_of_size(enabled: bool, size: i32) -> HICON {
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
