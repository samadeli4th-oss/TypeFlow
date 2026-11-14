//! Notification-area (tray) icon, its menu, and the runtime settings it toggles.
//!
//! Left click: turn hints on/off. Right click: menu.
//! The icon is drawn at runtime (a rounded square with "ف"), so no .ico file
//! or resource compiler is needed. Blue = on, grey = off.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::Settings;
use crate::win::FA_FONT;

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
    let tip = if get(&ENABLED) { "Smart Keyboard: on" } else { "Smart Keyboard: off" };
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
        item(MF_STRING | MF_GRAYED, 0, w!("Smart Keyboard  ·  English ⇄ فارسی"));
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

/// Premultiplied BGRA pixels of the icon, `size` x `size`, top row first.
pub fn render_icon(size: i32, enabled: bool) -> Vec<u32> {
    let n = (size * size) as usize;
    let glyph = render_glyph(size);

    let (br, bg, bb) = if enabled { (37.0, 99.0, 235.0) } else { (130.0, 130.0, 130.0) };
    let radius = size as f64 * 0.24;
    let s = size as f64;
    let inside = |x: f64, y: f64| {
        let dx = (radius - x).max(x - (s - radius)).max(0.0);
        let dy = (radius - y).max(y - (s - radius)).max(0.0);
        dx * dx + dy * dy <= radius * radius
    };

    let mut out = vec![0u32; n];
    for y in 0..size {
        for x in 0..size {
            // rounded-square coverage, 4x4 supersampled for smooth corners
            let mut hits = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    if inside(x as f64 + (sx as f64 + 0.5) / 4.0, y as f64 + (sy as f64 + 0.5) / 4.0) {
                        hits += 1;
                    }
                }
            }
            let a = hits as f64 / 16.0;
            let i = (y * size + x) as usize;
            let t = glyph[i]; // 0 = background, 1 = white letter
            let mix = |c: f64| (c + (255.0 - c) * t) * a;
            out[i] = ((a * 255.0).round() as u32) << 24
                | (mix(br).round() as u32) << 16
                | (mix(bg).round() as u32) << 8
                | mix(bb).round() as u32;
        }
    }
    out
}

/// Coverage (0..1) of the white "ف", fitted into the middle ~70% of the icon.
fn render_glyph(size: i32) -> Vec<f64> {
    // Persian glyph metrics vary a lot, so measure: draw once on a big canvas,
    // scale the font so the ink fits, draw again and centre the ink.
    let canvas = size * 3;
    let probe = size as f64;
    let (bw, bh) = ink_box(&draw_glyph(canvas, probe), canvas).map_or((1, 1), |(x0, y0, x1, y1)| (x1 - x0, y1 - y0));
    let target = size as f64 * 0.7;
    let height = probe * (target / bw as f64).min(target / bh as f64);
    let big = draw_glyph(canvas, height);
    let Some((x0, y0, x1, y1)) = ink_box(&big, canvas) else { return vec![0.0; (size * size) as usize] };
    let (dx, dy) = ((size - (x1 - x0)) / 2 - x0, (size - (y1 - y0)) / 2 - y0);
    let mut out = vec![0.0; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let (sx, sy) = (x - dx, y - dy);
            if (0..canvas).contains(&sx) && (0..canvas).contains(&sy) {
                out[(y * size + x) as usize] = big[(sy * canvas + sx) as usize];
            }
        }
    }
    out
}

/// Bounding box (x0, y0, x1, y1), exclusive end, of pixels with visible ink.
fn ink_box(cov: &[f64], size: i32) -> Option<(i32, i32, i32, i32)> {
    let mut b: Option<(i32, i32, i32, i32)> = None;
    for y in 0..size {
        for x in 0..size {
            if cov[(y * size + x) as usize] > 0.08 {
                b = Some(match b {
                    None => (x, y, x + 1, y + 1),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                });
            }
        }
    }
    b
}

/// White "ف" on black, centred in a `size` x `size` canvas, with a font of
/// `height` pixels; returns per-pixel coverage.
fn draw_glyph(size: i32, height: f64) -> Vec<f64> {
    let n = (size * size) as usize;
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, size, size);
        let old_bmp = SelectObject(dc, bmp);
        let font = CreateFontW(
            -(height.round() as i32).max(1), 0, 0, 0, 700, 0, 0, 0, DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, ANTIALIASED_QUALITY.0 as u32, 0, FA_FONT,
        );
        let old_font = SelectObject(dc, font);
        let mut rect = RECT { left: 0, top: 0, right: size, bottom: size };
        FillRect(dc, &rect, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        SetBkMode(dc, TRANSPARENT);
        let mut text: Vec<u16> = "ف".encode_utf16().collect();
        DrawTextW(dc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u32; n];
        SelectObject(dc, old_bmp);
        GetDIBits(dc, bmp, 0, size as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);

        SelectObject(dc, old_font);
        let _ = DeleteObject(font);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        pixels.iter().map(|p| ((p >> 8) & 0xFF) as f64 / 255.0).collect()
    }
}

fn make_icon(enabled: bool) -> HICON {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let pixels = render_icon(size, enabled);
        let screen = GetDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, pixels.len());
        }
        let mask_bits = vec![0u8; ((size + 15) / 16 * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: TRUE,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        })
        .unwrap_or_default();
        let _ = DeleteObject(mask);
        let _ = DeleteObject(color);
        ReleaseDC(None, screen);
        icon
    }
}
