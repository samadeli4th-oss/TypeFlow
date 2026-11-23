//! The TypeFlow window: a small flyout that opens beside the tray icon and
//! holds every setting, including auto-correct for each language.
//!
//! Like the Windows flyouts it closes on Esc or when it loses focus, and it
//! follows the light/dark mode and accent color (theme.rs). Commands go to the
//! tray window as WM_PANEL_COMMAND, so settings change in one place only.
//!
//! Shapes are drawn into a pixel buffer so they are anti-aliased; text is drawn
//! on top with GDI, which shapes Persian correctly.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::FieldAction;
use crate::theme::Theme;
use crate::tray;
use crate::userwords;

// ------------------------------------------------------------------ contents ---
struct Switch {
    cmd: usize,
    flag: &'static AtomicBool,
    title: &'static str,
    detail: &'static str,
    persian: bool,
}

/// One choice of a setting with several states.
struct Radio {
    cmd: usize,
    action: FieldAction,
    title: &'static str,
}

enum Row {
    Section(&'static str),
    Switch(Switch),
    Radio(Radio),
    /// Opens the Custom words window.
    Words,
    Exit,
}

const fn switch(cmd: usize, flag: &'static AtomicBool, title: &'static str, detail: &'static str, persian: bool) -> Row {
    Row::Switch(Switch { cmd, flag, title, detail, persian })
}

const fn radio(cmd: usize, action: FieldAction, title: &'static str) -> Row {
    Row::Radio(Radio { cmd, action, title })
}

static ROWS: [Row; 16] = [
    Row::Section("Layout switching"),
    switch(tray::CMD_ENABLED, &tray::ENABLED, "Layout hints", "Show a hint for the wrong layout", false),
    switch(tray::CMD_AUTO_FIX, &tray::AUTO_FIX, "Auto-fix when sure", "Convert without asking", false),
    switch(tray::CMD_LEARN, &tray::LEARN_WORDS, "Learn new words", "Keep words you reject 4 times", false),
    Row::Words,
    switch(tray::CMD_DING, &tray::DING, "Sound", "Play a sound for hints and typos", false),
    Row::Section("Auto-correct"),
    switch(tray::CMD_AUTOCORRECT_EN, &tray::AUTOCORRECT_EN, "English", "Suggest fixes for typos", false),
    switch(tray::CMD_AUTOCORRECT_FA, &tray::AUTOCORRECT_FA, "فارسی", "پیشنهاد اصلاح غلط‌های املایی", true),
    Row::Section("Password and number fields"),
    radio(tray::CMD_FIELD_ENGLISH, FieldAction::English, "Change keyboard to English"),
    radio(tray::CMD_FIELD_NOTIFY, FieldAction::Notify, "Notify"),
    radio(tray::CMD_FIELD_DISABLE, FieldAction::Disable, "Disable"),
    Row::Section("General"),
    switch(tray::CMD_AUTOSTART, &tray::START_WITH_WINDOWS, "Start with Windows", "Open TypeFlow when you sign in", false),
    Row::Exit,
];

// -------------------------------------------------------------------- layout ---
// Sizes are at 96 DPI.
const WIDTH: i32 = 320;
const HEADER: i32 = 56;
const SECTION: i32 = 30;
const SWITCH: i32 = 46;
const RADIO: i32 = 34;
const EXIT: i32 = 42;
const BOTTOM: i32 = 8;
const PAD: i32 = 16;
const ROW_INSET: i32 = 6;
const GAP: i32 = 12;
const TRACK_W: i32 = 40;
const TRACK_H: i32 = 20;

/// A click on the icon first closes the open window (it loses focus); a click
/// arriving this soon afterwards must not open it again.
const REOPEN_GUARD: Duration = Duration::from_millis(350);

fn px(v: i32, dpi: i32) -> i32 {
    v * dpi / 96
}

fn row_height(row: &Row) -> i32 {
    match row {
        Row::Section(_) => SECTION,
        Row::Switch(_) | Row::Words => SWITCH,
        Row::Radio(_) => RADIO,
        Row::Exit => EXIT,
    }
}

/// (top, bottom) of row `index`.
fn row_span(index: usize, dpi: i32) -> (i32, i32) {
    let top = HEADER + ROWS[..index].iter().map(row_height).sum::<i32>();
    (px(top, dpi), px(top + row_height(&ROWS[index]), dpi))
}

fn total_height(dpi: i32) -> i32 {
    px(HEADER + ROWS.iter().map(row_height).sum::<i32>() + BOTTOM, dpi)
}

/// The DPI to draw at so the whole window fits in `available` pixels.
fn fit_dpi(dpi: i32, available: i32) -> i32 {
    let mut fitted = dpi;
    while fitted > 72 && total_height(fitted) > available {
        fitted -= 1;
    }
    fitted
}

fn is_interactive(index: usize) -> bool {
    !matches!(ROWS[index], Row::Section(_))
}

/// The row a click at height `y` lands on, if it can be clicked.
fn row_at(y: i32, dpi: i32) -> Option<usize> {
    (0..ROWS.len()).find(|&i| {
        let (top, bottom) = row_span(i, dpi);
        is_interactive(i) && y >= top && y < bottom
    })
}

/// Next row for Tab / arrows, wrapping around.
fn next_focus(current: Option<usize>, forward: bool) -> Option<usize> {
    let rows: Vec<usize> = (0..ROWS.len()).filter(|&i| is_interactive(i)).collect();
    let n = rows.len();
    let at = current.and_then(|c| rows.iter().position(|&r| r == c));
    let next = match (at, forward) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(a), true) => (a + 1) % n,
        (Some(a), false) => (a + n - 1) % n,
    };
    rows.get(next).copied()
}

/// Where a `w` x `h` window goes for a tray icon at `icon`: against the taskbar,
/// lined up with the icon, and always inside the work area.
fn place(icon: RECT, w: i32, h: i32, work: RECT, monitor: RECT, gap: i32) -> (i32, i32) {
    let clamp = |v: i32, lo: i32, hi: i32| v.min(hi).max(lo);
    let (cx, cy) = ((icon.left + icon.right) / 2, (icon.top + icon.bottom) / 2);
    let (min_x, max_x) = (work.left + gap, work.right - gap - w);
    let (min_y, max_y) = (work.top + gap, work.bottom - gap - h);

    // The taskbar is where the work area stops short of the monitor. With an
    // auto-hidden taskbar they match, so use the edge nearest the icon.
    let distances = [cy - monitor.top, monitor.bottom - cy, cx - monitor.left, monitor.right - cx];
    let nearest = (0..4).min_by_key(|&i| distances[i]).unwrap_or(1);
    let edge = if work.bottom < monitor.bottom {
        1
    } else if work.top > monitor.top {
        0
    } else if work.left > monitor.left {
        2
    } else if work.right < monitor.right {
        3
    } else {
        nearest
    };
    match edge {
        0 => (clamp(cx - w / 2, min_x, max_x), min_y),
        1 => (clamp(cx - w / 2, min_x, max_x), max_y),
        2 => (min_x, clamp(cy - h / 2, min_y, max_y)),
        _ => (max_x, clamp(cy - h / 2, min_y, max_y)),
    }
}

// --------------------------------------------------------------------- state ---
#[derive(Clone, Copy)]
struct Panel {
    hwnd: isize,
    owner: isize,
    dpi: i32,
    theme: Theme,
    hover: Option<usize>,
    pressed: Option<usize>,
    focus: Option<usize>,
    tracking: bool,
    closed_at: Option<Instant>,
}

thread_local! {
    // A plain copyable value: Win32 calls can re-enter the window procedure,
    // so nothing stays borrowed across them.
    static PANEL: Cell<Panel> = const { Cell::new(Panel {
        hwnd: 0,
        owner: 0,
        dpi: 96,
        theme: Theme::DEFAULT,
        hover: None,
        pressed: None,
        focus: None,
        tracking: false,
        closed_at: None,
    }) };
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
}

fn get() -> Panel {
    PANEL.with(Cell::get)
}

fn update(f: impl FnOnce(&mut Panel)) {
    let mut p = get();
    f(&mut p);
    PANEL.with(|c| c.set(p));
}

fn handle(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

// ---------------------------------------------------------------- public API ---

/// True if `window` is the TypeFlow window.
pub fn is_panel(window: HWND) -> bool {
    let raw = get().hwnd;
    raw != 0 && raw == window.0 as isize
}

fn is_open() -> bool {
    let raw = get().hwnd;
    raw != 0 && unsafe { IsWindowVisible(handle(raw)) }.as_bool()
}

/// A left click on the tray icon.
pub fn toggle(owner: HWND) {
    if is_open() {
        hide();
    } else if get().closed_at.map_or(true, |at| at.elapsed() > REOPEN_GUARD) {
        show(owner);
    }
}

pub fn show(owner: HWND) {
    let Some(window) = ensure_window() else { return };
    unsafe {
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let icon = tray::icon_rect(owner).unwrap_or(RECT {
            left: cursor.x,
            top: cursor.y,
            right: cursor.x + 1,
            bottom: cursor.y + 1,
        });
        let monitor = MonitorFromRect(&icon, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let available = info.rcWork.bottom - info.rcWork.top - 2 * px(GAP, dpi_x as i32);
        let dpi = fit_dpi(dpi_x as i32, available);
        let theme = Theme::current();
        update(|p| {
            p.owner = owner.0 as isize;
            p.dpi = dpi;
            p.theme = theme;
            p.hover = None;
            p.pressed = None;
            p.focus = None;
            p.tracking = false;
        });
        crate::theme::apply(window, &theme);
        let (width, height) = (px(WIDTH, dpi), total_height(dpi));
        let (x, y) = place(icon, width, height, info.rcWork, info.rcMonitor, px(GAP, dpi));
        let _ = SetWindowPos(window, HWND_TOPMOST, x, y, width, height, SWP_SHOWWINDOW);
        let _ = InvalidateRect(window, None, false);
        let _ = SetForegroundWindow(window);
        let _ = SetFocus(window);
    }
}

pub fn hide() {
    if !is_open() {
        return;
    }
    update(|p| {
        p.closed_at = Some(Instant::now());
        p.hover = None;
        p.pressed = None;
        p.focus = None;
    });
    unsafe {
        let _ = ShowWindow(handle(get().hwnd), SW_HIDE);
    }
}

/// A setting changed: redraw if the window is open.
pub fn refresh() {
    if is_open() {
        unsafe {
            let _ = InvalidateRect(handle(get().hwnd), None, false);
        }
    }
}

fn ensure_window() -> Option<HWND> {
    let raw = get().hwnd;
    if raw != 0 {
        return Some(handle(raw));
    }
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(panel_proc),
            hInstance: instance.into(),
            lpszClassName: w!("TypeFlowPanel"),
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("TypeFlowPanel"),
            w!("TypeFlow"),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, instance, None,
        )
        .ok()?;
        update(|p| p.hwnd = window.0 as isize);
        Some(window)
    }
}

fn activate(index: usize) {
    let owner = handle(get().owner);
    let cmd = match &ROWS[index] {
        Row::Switch(s) => s.cmd,
        Row::Radio(r) => r.cmd,
        Row::Words => tray::CMD_WORDS,
        Row::Exit => tray::CMD_EXIT,
        Row::Section(_) => return,
    };
    unsafe {
        let _ = PostMessageW(owner, tray::WM_PANEL_COMMAND, WPARAM(cmd), LPARAM(0));
    }
}

// -------------------------------------------------------------- window proc ---
/// Not in the `windows` features this app enables.
const WM_MOUSELEAVE: u32 = 0x02A3;

fn redraw(window: HWND) {
    unsafe {
        let _ = InvalidateRect(window, None, false);
    }
}

unsafe extern "system" fn panel_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mouse_y = || ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    match msg {
        WM_PAINT => paint(window),
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEMOVE => {
            let row = row_at(mouse_y(), get().dpi);
            if !get().tracking {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
                update(|p| p.tracking = true);
            }
            if get().hover != row {
                update(|p| p.hover = row);
                redraw(window);
            }
        }
        WM_MOUSELEAVE => {
            update(|p| {
                p.tracking = false;
                p.hover = None;
                p.pressed = None;
            });
            redraw(window);
        }
        WM_LBUTTONDOWN => {
            let row = row_at(mouse_y(), get().dpi);
            update(|p| {
                p.pressed = row;
                p.focus = None;
            });
            redraw(window);
        }
        WM_LBUTTONUP => {
            let row = row_at(mouse_y(), get().dpi);
            let pressed = get().pressed;
            update(|p| p.pressed = None);
            redraw(window);
            if let Some(i) = row.filter(|_| row == pressed) {
                activate(i);
            }
        }
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wparam.0 as u16);
            match vk {
                VK_ESCAPE => hide(),
                VK_TAB | VK_DOWN | VK_UP => {
                    let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    let forward = vk == VK_DOWN || (vk == VK_TAB && !shift);
                    update(|p| p.focus = next_focus(p.focus, forward));
                    redraw(window);
                }
                VK_SPACE | VK_RETURN => {
                    if let Some(i) = get().focus {
                        activate(i);
                    }
                }
                _ => {}
            }
        }
        WM_ACTIVATE => {
            if wparam.0 & 0xFFFF == 0 {
                hide(); // WA_INACTIVE: clicked somewhere else
            }
        }
        WM_CLOSE => hide(),
        _ => return DefWindowProcW(window, msg, wparam, lparam),
    }
    LRESULT(0)
}

// ------------------------------------------------------------------- drawing ---
pub(crate) type Rgb = (u8, u8, u8);

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn luminance(c: Rgb) -> f32 {
    (0.2126 * c.0 as f32 + 0.7152 * c.1 as f32 + 0.0722 * c.2 as f32) / 255.0
}

#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) bg: Rgb,
    pub(crate) text: Rgb,
    pub(crate) muted: Rgb,
    pub(crate) line: Rgb,
    pub(crate) hover: Rgb,
    pub(crate) pressed: Rgb,
    pub(crate) accent: Rgb,
    pub(crate) on_accent: Rgb,
}

pub(crate) fn palette(theme: &Theme) -> Palette {
    let c = theme.colors();
    let (bg, text) = (c.fill, c.text);
    // keep the "on" switch visible against the window in both modes
    let accent = if theme.light {
        if luminance(theme.accent) > 0.45 { mix(theme.accent, (0, 0, 0), 0.35) } else { theme.accent }
    } else if luminance(theme.accent) < 0.4 {
        mix(theme.accent, (255, 255, 255), 0.45)
    } else {
        theme.accent
    };
    Palette {
        bg,
        text,
        muted: mix(text, bg, 0.35),
        line: mix(bg, text, 0.14),
        hover: mix(bg, text, 0.07),
        pressed: mix(bg, text, 0.12),
        accent,
        on_accent: if luminance(accent) > 0.5 { (0, 0, 0) } else { (255, 255, 255) },
    }
}

pub(crate) struct Canvas<'a> {
    pub(crate) px: &'a mut [u32],
    pub(crate) w: i32,
    pub(crate) h: i32,
}

impl Canvas<'_> {
    pub(crate) fn clear(&mut self, c: Rgb) {
        self.px.fill((c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32);
    }

    /// Anti-aliased rounded rectangle; with `stroke`, only an outline that wide.
    pub(crate) fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb, stroke: Option<f32>) {
        let (cx, cy, hw, hh) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let (x0, x1) = ((x.floor() as i32 - 1).max(0), ((x + w).ceil() as i32 + 1).min(self.w));
        let (y0, y1) = ((y.floor() as i32 - 1).max(0), ((y + h).ceil() as i32 + 1).min(self.h));
        for py in y0..y1 {
            for px in x0..x1 {
                let qx = (px as f32 + 0.5 - cx).abs() - (hw - r);
                let qy = (py as f32 + 0.5 - cy).abs() - (hh - r);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
                let cover = match stroke {
                    Some(t) => (0.5 - (d + t / 2.0).abs() + t / 2.0).clamp(0.0, 1.0),
                    None => (0.5 - d).clamp(0.0, 1.0),
                };
                if cover > 0.0 {
                    let i = (py * self.w + px) as usize;
                    let v = self.px[i];
                    let under = ((v >> 16) as u8, (v >> 8) as u8, v as u8);
                    let o = mix(under, c, cover);
                    self.px[i] = (o.0 as u32) << 16 | (o.1 as u32) << 8 | o.2 as u32;
                }
            }
        }
    }
}

struct Fonts {
    dpi: i32,
    title: HFONT,
    section: HFONT,
    body: HFONT,
    small: HFONT,
    fa_body: HFONT,
    fa_small: HFONT,
}

impl Fonts {
    unsafe fn new(dpi: i32) -> Fonts {
        let make = |size: i32, weight: i32, face: PCWSTR| {
            CreateFontW(-px(size, dpi), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, face)
        };
        let (en, fa) = (w!("Segoe UI"), w!("Vazirmatn RD Light"));
        Fonts {
            dpi,
            title: make(17, 600, en),
            section: make(12, 600, en),
            body: make(14, 400, en),
            small: make(12, 400, en),
            fa_body: make(14, 400, fa),
            fa_small: make(12, 400, fa),
        }
    }

    unsafe fn delete(&self) {
        for font in [self.title, self.section, self.body, self.small, self.fa_body, self.fa_small] {
            let _ = DeleteObject(font);
        }
    }
}

fn with_fonts<R>(dpi: i32, f: impl FnOnce(&Fonts) -> R) -> R {
    FONTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_some_and(|fonts| fonts.dpi != dpi) {
            if let Some(old) = slot.take() {
                unsafe { old.delete() };
            }
        }
        f(slot.get_or_insert_with(|| unsafe { Fonts::new(dpi) }))
    })
}

pub(crate) unsafe fn text(dc: HDC, font: HFONT, s: &str, mut rect: RECT, color: Rgb, flags: DRAW_TEXT_FORMAT) {
    SelectObject(dc, font);
    SetTextColor(dc, COLORREF(color.0 as u32 | (color.1 as u32) << 8 | (color.2 as u32) << 16));
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    DrawTextW(dc, &mut buf, &mut rect, flags | DT_SINGLELINE | DT_NOPREFIX | DT_VCENTER | DT_END_ELLIPSIS);
}

fn draw_shapes(canvas: &mut Canvas, p: &Panel, c: &Palette) {
    let dpi = p.dpi;
    let f = |v: i32| px(v, dpi) as f32;
    canvas.clear(c.bg);
    let width = canvas.w as f32;
    for (i, row) in ROWS.iter().enumerate() {
        let (top, bottom) = row_span(i, dpi);
        let (top, height) = (top as f32, (bottom - top) as f32);
        if is_interactive(i) {
            let (x, w) = (f(ROW_INSET), width - 2.0 * f(ROW_INSET));
            let (y, h) = (top + f(1), height - f(2));
            if p.pressed == Some(i) {
                canvas.round_rect(x, y, w, h, f(5), c.pressed, None);
            } else if p.hover == Some(i) {
                canvas.round_rect(x, y, w, h, f(5), c.hover, None);
            }
            if p.focus == Some(i) {
                canvas.round_rect(x, y, w, h, f(5), c.text, Some(f(2).max(1.5)));
            }
        }
        if let Row::Switch(s) = row {
            let on = tray::get(s.flag);
            let (tw, th) = (f(TRACK_W), f(TRACK_H));
            let (tx, ty) = (width - f(PAD) - tw, top + (height - th) / 2.0);
            let center_y = ty + th / 2.0;
            if on {
                canvas.round_rect(tx, ty, tw, th, th / 2.0, c.accent, None);
                let r = f(7);
                canvas.round_rect(tx + tw - f(10) - r, center_y - r, r * 2.0, r * 2.0, r, c.on_accent, None);
            } else {
                canvas.round_rect(tx, ty, tw, th, th / 2.0, c.muted, Some(f(1).max(1.0)));
                let r = f(5);
                canvas.round_rect(tx + f(10) - r, center_y - r, r * 2.0, r * 2.0, r, c.muted, None);
            }
        }
        if let Row::Radio(r) = row {
            let radius = f(9);
            let (cx, cy) = (width - f(PAD) - radius, top + height / 2.0);
            if tray::field_action() == r.action {
                canvas.round_rect(cx - radius, cy - radius, radius * 2.0, radius * 2.0, radius, c.accent, None);
                let dot = f(4);
                canvas.round_rect(cx - dot, cy - dot, dot * 2.0, dot * 2.0, dot, c.on_accent, None);
            } else {
                canvas.round_rect(cx - radius, cy - radius, radius * 2.0, radius * 2.0, radius, c.muted, Some(f(1).max(1.0)));
            }
        }
        if matches!(row, Row::Exit) {
            canvas.round_rect(f(PAD), top, width - 2.0 * f(PAD), f(1).max(1.0), 0.0, c.line, None);
        }
    }
}

unsafe fn draw_text(dc: HDC, width: i32, p: &Panel, c: &Palette) {
    let dpi = p.dpi;
    let v = |n: i32| px(n, dpi);
    SetBkMode(dc, TRANSPARENT);
    with_fonts(dpi, |fonts| {
        let icon_size = v(24);
        let icon = tray::icon_of_size(tray::get(&tray::ENABLED), icon_size);
        if !icon.is_invalid() {
            let _ = DrawIconEx(dc, v(PAD), (v(HEADER) - icon_size) / 2, icon, icon_size, icon_size, 0, None, DI_NORMAL);
            let _ = DestroyIcon(icon);
        }
        let header = |left, right| RECT { left, top: 0, right, bottom: v(HEADER) };
        text(dc, fonts.title, "TypeFlow", header(v(PAD) + icon_size + v(10), width - v(70)), c.text, DT_LEFT);
        text(dc, fonts.small, concat!("v", env!("CARGO_PKG_VERSION")), header(width - v(90), width - v(PAD)), c.muted, DT_RIGHT);

        for (i, row) in ROWS.iter().enumerate() {
            let (top, bottom) = row_span(i, dpi);
            let left = v(PAD);
            match row {
                Row::Section(title) => {
                    let r = RECT { left, top: top + v(6), right: width - v(PAD), bottom };
                    text(dc, fonts.section, title, r, c.muted, DT_LEFT);
                }
                Row::Switch(s) => {
                    let right = width - v(PAD) - v(TRACK_W) - v(12);
                    let middle = top + (bottom - top) / 2;
                    let upper = RECT { left, top: top + v(3), right, bottom: middle + v(1) };
                    let lower = RECT { left, top: middle, right, bottom: bottom - v(3) };
                    let (title_font, detail_font, flags) = if s.persian {
                        (fonts.fa_body, fonts.fa_small, DT_LEFT | DT_RTLREADING)
                    } else {
                        (fonts.body, fonts.small, DT_LEFT)
                    };
                    text(dc, title_font, s.title, upper, c.text, flags);
                    text(dc, detail_font, s.detail, lower, c.muted, flags);
                }
                Row::Radio(choice) => {
                    let r = RECT { left, top, right: width - v(PAD) - v(30), bottom };
                    text(dc, fonts.body, choice.title, r, c.text, DT_LEFT);
                }
                Row::Words => {
                    let right = width - v(PAD) - v(TRACK_W) - v(12);
                    let middle = top + (bottom - top) / 2;
                    let upper = RECT { left, top: top + v(3), right, bottom: middle + v(1) };
                    let lower = RECT { left, top: middle, right, bottom: bottom - v(3) };
                    text(dc, fonts.body, "Custom words", upper, c.text, DT_LEFT);
                    text(dc, fonts.small, "Add or remove valid words", lower, c.muted, DT_LEFT);
                    let count = RECT { left: right, top, right: width - v(PAD), bottom };
                    text(dc, fonts.body, &format!("{}  ›", userwords::count()), count, c.muted, DT_RIGHT);
                }
                Row::Exit => {
                    let r = RECT { left, top, right: width - v(PAD), bottom };
                    text(dc, fonts.body, "Exit TypeFlow", r, c.text, DT_LEFT);
                }
            }
        }
    });
}

unsafe fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(window, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(window, &mut client);
    let (w, h) = (client.right, client.bottom);
    let panel = get();
    let colors = palette(&panel.theme);

    let dc = CreateCompatibleDC(hdc);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    if let Ok(bitmap) = CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        let old = SelectObject(dc, bitmap);
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
        draw_shapes(&mut Canvas { px: pixels, w, h }, &panel, &colors);
        let _ = GdiFlush();
        draw_text(dc, w, &panel, &colors);
        let _ = BitBlt(hdc, 0, 0, w, h, dc, 0, 0, SRCCOPY);
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap);
    }
    let _ = DeleteDC(dc);
    let _ = EndPaint(window, &ps);
}

// -------------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    const SCREEN: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };

    #[test]
    fn opens_above_a_bottom_taskbar_beside_the_icon() {
        let work = rect(0, 0, 1920, 1032);
        assert_eq!(place(rect(1600, 1040, 1624, 1064), 320, 460, work, SCREEN, 12), (1452, 560));
    }

    #[test]
    fn stays_on_screen_for_an_icon_in_the_corner() {
        let work = rect(0, 0, 1920, 1032);
        assert_eq!(place(rect(1890, 1040, 1914, 1064), 320, 460, work, SCREEN, 12), (1588, 560));
    }

    #[test]
    fn follows_the_taskbar_on_other_edges() {
        let top = rect(0, 48, 1920, 1080);
        assert_eq!(place(rect(1600, 10, 1624, 34), 320, 460, top, SCREEN, 12), (1452, 60));
        let left = rect(48, 0, 1920, 1080);
        assert_eq!(place(rect(10, 900, 34, 924), 320, 460, left, SCREEN, 12), (60, 608));
        let right = rect(0, 0, 1872, 1080);
        assert_eq!(place(rect(1885, 900, 1909, 924), 320, 460, right, SCREEN, 12), (1540, 608));
    }

    #[test]
    fn handles_an_auto_hidden_taskbar_and_a_second_monitor() {
        assert_eq!(place(rect(1600, 1050, 1624, 1074), 320, 460, SCREEN, SCREEN, 12), (1452, 608));
        let second = rect(1920, 0, 3840, 1080);
        let work = rect(1920, 0, 3840, 1032);
        assert_eq!(place(rect(3500, 1040, 3524, 1064), 320, 460, work, second, 12), (3352, 560));
    }

    #[test]
    fn each_language_has_its_own_switch() {
        let commands: Vec<usize> = ROWS.iter().filter_map(|r| match r {
            Row::Switch(s) => Some(s.cmd),
            _ => None,
        }).collect();
        assert!(commands.contains(&tray::CMD_AUTOCORRECT_EN));
        assert!(commands.contains(&tray::CMD_AUTOCORRECT_FA));
    }

    #[test]
    fn sections_cannot_be_clicked() {
        let (top, _) = row_span(0, 96);
        assert_eq!(row_at(top + 1, 96), None);
        let (top, _) = row_span(1, 96);
        assert_eq!(row_at(top + 1, 96), Some(1));
    }

    #[test]
    fn keyboard_focus_skips_sections_and_wraps() {
        assert_eq!(next_focus(None, true), Some(1));
        assert_eq!(next_focus(Some(5), true), Some(7));
        assert_eq!(next_focus(Some(12), true), Some(14));
        assert_eq!(next_focus(Some(15), true), Some(1));
        assert_eq!(next_focus(Some(1), false), Some(15));
    }

    #[test]
    fn the_field_setting_has_its_three_choices() {
        let titles: Vec<&str> = ROWS.iter().filter_map(|r| match r {
            Row::Radio(choice) => Some(choice.title),
            _ => None,
        }).collect();
        assert_eq!(titles, ["Change keyboard to English", "Notify", "Disable"]);
    }

    #[test]
    fn shrinks_to_fit_a_small_screen() {
        assert_eq!(fit_dpi(96, 2000), 96);
        let fitted = fit_dpi(144, total_height(96));
        assert!(fitted < 144 && total_height(fitted) <= total_height(96));
    }

    #[test]
    fn rows_scale_with_dpi() {
        assert_eq!(total_height(192), total_height(96) * 2);
    }

    #[test]
    fn the_on_switch_is_readable_in_both_modes() {
        for light in [true, false] {
            for accent in [(0, 120, 212), (255, 220, 0), (20, 20, 90)] {
                let c = palette(&Theme { light, accent, ..Theme::DEFAULT });
                let gap = (luminance(c.accent) - luminance(c.bg)).abs();
                assert!(gap > 0.15, "accent {accent:?} light={light}");
                assert!((luminance(c.on_accent) - luminance(c.accent)).abs() > 0.3, "thumb on {accent:?}");
            }
        }
    }
}
