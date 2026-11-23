//! The auto-correct list: suggestions sorted by confidence, shown next to the
//! text cursor. It never takes focus, so typing carries on in the app.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::panel::{palette, Canvas};
use crate::spell::Suggestion;
use crate::theme::{self, Theme};
use crate::win::{place_bubble, WM_APP_SPELL_HOVER, WM_APP_SPELL_PICK};

// Sizes are at 96 DPI.
const ROW: i32 = 30;
const V_PAD: i32 = 4;
const H_PAD: i32 = 12;
const INSET: i32 = 4;
const PERCENT: i32 = 42;
const MIN_WIDTH: i32 = 160;
const MAX_WIDTH: i32 = 340;

fn px(v: i32, dpi: i32) -> i32 {
    v * dpi / 96
}

struct View {
    items: Vec<Suggestion>,
    selected: usize,
    rtl: bool,
    dpi: i32,
    theme: Theme,
}

struct Fonts {
    dpi: i32,
    en: HFONT,
    fa: HFONT,
    small: HFONT,
}

thread_local! {
    static WINDOW: Cell<isize> = const { Cell::new(0) };
    static OWNER: Cell<isize> = const { Cell::new(0) };
    static VIEW: RefCell<View> = const { RefCell::new(View {
        items: Vec::new(),
        selected: 0,
        rtl: false,
        dpi: 96,
        theme: Theme::DEFAULT,
    }) };
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
}

fn with_fonts<R>(dpi: i32, f: impl FnOnce(&Fonts) -> R) -> R {
    FONTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_some_and(|fonts| fonts.dpi != dpi) {
            if let Some(old) = slot.take() {
                unsafe {
                    let _ = DeleteObject(old.en);
                    let _ = DeleteObject(old.fa);
                    let _ = DeleteObject(old.small);
                }
            }
        }
        f(slot.get_or_insert_with(|| unsafe {
            let make = |size: i32, face: PCWSTR| {
                CreateFontW(-px(size, dpi), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET.0 as u32,
                    OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, face)
            };
            Fonts { dpi, en: make(15, w!("Segoe UI")), fa: make(14, w!("Vazirmatn RD Light")), small: make(11, w!("Segoe UI")) }
        }))
    })
}

fn percent(confidence: f32) -> String {
    match (confidence * 100.0).round() as i32 {
        n if n < 1 => "<1%".to_string(),
        n => format!("{}%", n.min(99)),
    }
}

fn ensure_window(owner: HWND) -> Option<HWND> {
    OWNER.with(|o| o.set(owner.0 as isize));
    let raw = WINDOW.with(Cell::get);
    if raw != 0 {
        return Some(HWND(raw as *mut c_void));
    }
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(list_proc),
            hInstance: instance.into(),
            lpszClassName: w!("TypeFlowSuggest"),
            hCursor: LoadCursorW(None, IDC_HAND).ok()?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("TypeFlowSuggest"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, instance, None,
        )
        .ok()?;
        WINDOW.with(|c| c.set(window.0 as isize));
        Some(window)
    }
}

unsafe fn list_width(items: &[Suggestion], rtl: bool, dpi: i32) -> i32 {
    with_fonts(dpi, |fonts| {
        let dc = CreateCompatibleDC(None);
        SelectObject(dc, if rtl { fonts.fa } else { fonts.en });
        let flags = DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX | if rtl { DT_RTLREADING } else { DT_LEFT };
        let widest = items.iter().map(|item| {
            let mut buf: Vec<u16> = item.word.encode_utf16().collect();
            let mut r = RECT::default();
            DrawTextW(dc, &mut buf, &mut r, flags);
            r.right
        }).max().unwrap_or(0);
        let _ = DeleteDC(dc);
        (widest + px(2 * H_PAD + PERCENT, dpi)).clamp(px(MIN_WIDTH, dpi), px(MAX_WIDTH, dpi))
    })
}

/// Shows the list by the caret and returns where it is on screen.
pub fn show(owner: HWND, items: &[Suggestion], selected: usize, rtl: bool, caret: RECT, work: RECT) -> Option<RECT> {
    let window = ensure_window(owner)?;
    unsafe {
        let monitor = MonitorFromRect(&caret, MONITOR_DEFAULTTONEAREST);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi = dpi_x as i32;
        let theme = Theme::current();
        VIEW.with(|v| *v.borrow_mut() = View { items: items.to_vec(), selected, rtl, dpi, theme });
        theme::apply(window, &theme);
        let width = list_width(items, rtl, dpi);
        let height = px(ROW * items.len() as i32 + 2 * V_PAD, dpi);
        let (x, y) = place_bubble(caret, width, height, work, rtl);
        let _ = SetWindowPos(window, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(window, None, false);
        Some(RECT { left: x, top: y, right: x + width, bottom: y + height })
    }
}

pub fn select(selected: usize) {
    VIEW.with(|v| v.borrow_mut().selected = selected);
    let raw = WINDOW.with(Cell::get);
    if raw != 0 {
        unsafe {
            let _ = InvalidateRect(HWND(raw as *mut c_void), None, false);
        }
    }
}

pub fn hide() {
    let raw = WINDOW.with(Cell::get);
    if raw != 0 {
        unsafe {
            let _ = ShowWindow(HWND(raw as *mut c_void), SW_HIDE);
        }
    }
}

fn row_at(y: i32) -> Option<usize> {
    VIEW.with(|v| {
        let v = v.borrow();
        let inside = y - px(V_PAD, v.dpi);
        if inside < 0 {
            return None;
        }
        let row = (inside / px(ROW, v.dpi).max(1)) as usize;
        (row < v.items.len()).then_some(row)
    })
}

unsafe extern "system" fn list_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let row = || row_at(((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    let owner = || HWND(OWNER.with(Cell::get) as *mut c_void);
    match msg {
        WM_PAINT => paint(window),
        WM_ERASEBKGND => return LRESULT(1),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            if let Some(i) = row() {
                if VIEW.with(|v| v.borrow().selected) != i {
                    let _ = PostMessageW(owner(), WM_APP_SPELL_HOVER, WPARAM(i), LPARAM(0));
                }
            }
        }
        WM_LBUTTONDOWN => {
            if let Some(i) = row() {
                let _ = PostMessageW(owner(), WM_APP_SPELL_PICK, WPARAM(i), LPARAM(0));
            }
        }
        _ => return DefWindowProcW(window, msg, wparam, lparam),
    }
    LRESULT(0)
}

unsafe fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(window, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(window, &mut client);
    let (w, h) = (client.right, client.bottom);
    let dc = CreateCompatibleDC(hdc);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
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
        VIEW.with(|v| draw(dc, pixels, w, h, &v.borrow()));
        let _ = BitBlt(hdc, 0, 0, w, h, dc, 0, 0, SRCCOPY);
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap);
    }
    let _ = DeleteDC(dc);
    let _ = EndPaint(window, &ps);
}

unsafe fn draw(dc: HDC, pixels: &mut [u32], w: i32, h: i32, view: &View) {
    let dpi = view.dpi;
    let v = |n: i32| px(n, dpi);
    let colors = palette(&view.theme);
    let mut canvas = Canvas { px: pixels, w, h };
    canvas.clear(colors.bg);
    let row_top = |i: usize| v(V_PAD) + v(ROW) * i as i32;
    let top = row_top(view.selected) as f32;
    let (x, width, height) = (v(INSET) as f32, (w - 2 * v(INSET)) as f32, (v(ROW) - v(2)) as f32);
    canvas.round_rect(x, top + v(1) as f32, width, height, v(5) as f32, colors.hover, None);
    let bar_x = if view.rtl { (w - v(INSET) - v(3)) as f32 } else { x };
    canvas.round_rect(bar_x, top + v(8) as f32, v(3) as f32, (v(ROW) - v(16)) as f32, v(1) as f32, colors.accent, None);
    let _ = GdiFlush();

    SetBkMode(dc, TRANSPARENT);
    let set_color = |c: (u8, u8, u8)| {
        SetTextColor(dc, COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16));
    };
    with_fonts(dpi, |fonts| {
        for (i, item) in view.items.iter().enumerate() {
            let (y0, y1) = (row_top(i), row_top(i + 1));
            let (word_rect, percent_rect, word_flags, percent_flags) = if view.rtl {
                (
                    RECT { left: v(H_PAD) + v(PERCENT), top: y0, right: w - v(H_PAD), bottom: y1 },
                    RECT { left: v(H_PAD), top: y0, right: v(H_PAD) + v(PERCENT), bottom: y1 },
                    DT_RIGHT | DT_RTLREADING,
                    DT_LEFT,
                )
            } else {
                (
                    RECT { left: v(H_PAD), top: y0, right: w - v(H_PAD) - v(PERCENT), bottom: y1 },
                    RECT { left: w - v(H_PAD) - v(PERCENT), top: y0, right: w - v(H_PAD), bottom: y1 },
                    DT_LEFT,
                    DT_RIGHT,
                )
            };
            let common = DT_SINGLELINE | DT_NOPREFIX | DT_VCENTER;
            SelectObject(dc, if view.rtl { fonts.fa } else { fonts.en });
            set_color(colors.text);
            let mut word: Vec<u16> = item.word.encode_utf16().collect();
            let mut r = word_rect;
            DrawTextW(dc, &mut word, &mut r, word_flags | common | DT_END_ELLIPSIS);
            SelectObject(dc, fonts.small);
            set_color(colors.muted);
            let mut text: Vec<u16> = percent(item.confidence).encode_utf16().collect();
            let mut r = percent_rect;
            DrawTextW(dc, &mut text, &mut r, percent_flags | common);
        }
    });
}
