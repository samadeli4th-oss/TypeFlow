//! The Custom words window: pick a language, type a word, and it stops being
//! flagged. Lists every added and learned word, each with a remove button.
//!
//! Drawn like the TypeFlow window (panel.rs) and following the Windows colors.
//! The only native control is the text box. Keys typed here are never recorded
//! by the keyboard hook (see `owns`).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::detect::Lang;
use crate::panel::{self, palette, text, Canvas, Palette};
use crate::theme::Theme;
use crate::tray;
use crate::userwords::{self, Entry, WordError};

// Sizes are at 96 DPI.
const WIDTH: i32 = 380;
const HEIGHT: i32 = 500;
const PAD: i32 = 16;
const LANG_TOP: i32 = 14;
const LANG_H: i32 = 34;
const FIELD_TOP: i32 = 60;
const FIELD_H: i32 = 36;
const ADD_W: i32 = 72;
const STATUS_TOP: i32 = 100;
const STATUS_H: i32 = 24;
const HEADER_TOP: i32 = 128;
const HEADER_H: i32 = 22;
const LIST_TOP: i32 = 152;
const ROW: i32 = 34;
const REMOVE_W: i32 = 30;
const LEARNED_W: i32 = 56;

const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0);
const ES_AUTOHSCROLL_STYLE: u32 = 0x0080;
const EM_SETLIMITTEXT_MSG: u32 = 0x00C5;
const MAX_INPUT: usize = 64;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;

fn px(v: i32, dpi: i32) -> i32 {
    v * dpi / 96
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Language(Lang),
    Add,
    Row(usize),
    Remove(usize),
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Edit,
    Language,
    List,
}

struct View {
    hwnd: isize,
    edit: isize,
    dpi: i32,
    theme: Theme,
    lang: Lang,
    items: Vec<Entry>,
    /// First row on screen.
    top: usize,
    selected: Option<usize>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    focus: Focus,
    tracking: bool,
    status: String,
    status_error: bool,
}

struct Fonts {
    dpi: i32,
    section: HFONT,
    body: HFONT,
    small: HFONT,
    fa_body: HFONT,
}

thread_local! {
    // Never borrowed across a Win32 call: those can re-enter the window procedure.
    static VIEW: RefCell<View> = const { RefCell::new(View {
        hwnd: 0,
        edit: 0,
        dpi: 96,
        theme: Theme::DEFAULT,
        lang: Lang::En,
        items: Vec::new(),
        top: 0,
        selected: None,
        hover: None,
        pressed: None,
        focus: Focus::Edit,
        tracking: false,
        status: String::new(),
        status_error: false,
    }) };
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
    static EDIT_PROC: Cell<isize> = const { Cell::new(0) };
    static EDIT_BRUSH: Cell<isize> = const { Cell::new(0) };
}

fn view<R>(f: impl FnOnce(&mut View) -> R) -> R {
    VIEW.with(|v| f(&mut v.borrow_mut()))
}

fn handle(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn with_fonts<R>(dpi: i32, f: impl FnOnce(&Fonts) -> R) -> R {
    FONTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_some_and(|fonts| fonts.dpi != dpi) {
            if let Some(old) = slot.take() {
                unsafe {
                    for font in [old.section, old.body, old.small, old.fa_body] {
                        let _ = DeleteObject(font);
                    }
                }
            }
        }
        f(slot.get_or_insert_with(|| unsafe {
            let make = |size: i32, weight: i32, face: PCWSTR| {
                CreateFontW(-px(size, dpi), 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET.0 as u32,
                    OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, face)
            };
            let (en, fa) = (w!("Segoe UI"), w!("Vazirmatn RD Light"));
            Fonts {
                dpi,
                section: make(12, 600, en),
                body: make(14, 400, en),
                small: make(12, 400, en),
                fa_body: make(14, 400, fa),
            }
        }))
    })
}

// -------------------------------------------------------------------- layout ---
struct Layout {
    language: RECT,
    field: RECT,
    add: RECT,
    status: RECT,
    header: RECT,
    list: RECT,
}

fn layout(w: i32, h: i32, dpi: i32) -> Layout {
    let v = |n: i32| px(n, dpi);
    let (left, right) = (v(PAD), w - v(PAD));
    let band = |top: i32, height: i32| RECT { left, top: v(top), right, bottom: v(top + height) };
    let add = RECT { left: right - v(ADD_W), top: v(FIELD_TOP), right, bottom: v(FIELD_TOP + FIELD_H) };
    Layout {
        language: band(LANG_TOP, LANG_H),
        field: RECT { left, top: v(FIELD_TOP), right: add.left - v(8), bottom: v(FIELD_TOP + FIELD_H) },
        add,
        status: band(STATUS_TOP, STATUS_H),
        header: band(HEADER_TOP, HEADER_H),
        list: RECT { left, top: v(LIST_TOP), right, bottom: (h - v(12)).max(v(LIST_TOP + ROW)) },
    }
}

fn visible_rows(list: &RECT, dpi: i32) -> usize {
    ((list.bottom - list.top) / px(ROW, dpi).max(1)).max(1) as usize
}

fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// The (English, Persian) halves of the language switch.
fn language_halves(r: &RECT) -> (RECT, RECT) {
    let middle = (r.left + r.right) / 2;
    (RECT { right: middle, ..*r }, RECT { left: middle, ..*r })
}

fn hit_at(x: i32, y: i32, w: i32, h: i32) -> Option<Hit> {
    let (dpi, top, count) = view(|v| (v.dpi, v.top, v.items.len()));
    let l = layout(w, h, dpi);
    if inside(&l.language, x, y) {
        let (english, _) = language_halves(&l.language);
        return Some(Hit::Language(if x < english.right { Lang::En } else { Lang::Fa }));
    }
    if inside(&l.add, x, y) {
        return Some(Hit::Add);
    }
    if inside(&l.list, x, y) {
        let row = ((y - l.list.top) / px(ROW, dpi).max(1)) as usize;
        let index = top + row;
        if row < visible_rows(&l.list, dpi) && index < count {
            return Some(if x >= l.list.right - px(REMOVE_W, dpi) { Hit::Remove(index) } else { Hit::Row(index) });
        }
    }
    None
}

fn client_size(window: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(window, &mut r) };
    (r.right, r.bottom)
}

// ---------------------------------------------------------------- public API ---

/// True if `window` is this window, so the keyboard hook leaves it alone.
pub fn owns(window: HWND) -> bool {
    let raw = view(|v| v.hwnd);
    raw != 0 && raw == window.0 as isize
}

fn is_open() -> bool {
    let raw = view(|v| v.hwnd);
    raw != 0 && unsafe { IsWindowVisible(handle(raw)) }.as_bool()
}

pub fn show() {
    let Some(window) = ensure_window() else { return };
    unsafe {
        if is_open() {
            let _ = SetForegroundWindow(window);
            return;
        }
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let dpi = dpi_x as i32;
        let theme = Theme::current();
        set_theme(window, theme);
        let items = userwords::entries();
        view(|v| {
            v.dpi = dpi;
            v.items = items;
            v.top = 0;
            v.selected = None;
            v.hover = None;
            v.pressed = None;
            v.focus = Focus::Edit;
            v.status.clear();
            v.status_error = false;
        });
        let mut frame = RECT { left: 0, top: 0, right: px(WIDTH, dpi), bottom: px(HEIGHT, dpi) };
        let _ = AdjustWindowRectExForDpi(&mut frame, STYLE, false, WINDOW_EX_STYLE(0), dpi as u32);
        let work = info.rcWork;
        let width = (frame.right - frame.left).min(work.right - work.left);
        let height = (frame.bottom - frame.top).min(work.bottom - work.top);
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        let _ = SetWindowPos(window, HWND_TOP, x, y, width, height, SWP_SHOWWINDOW);
        place_edit();
        let _ = InvalidateRect(window, None, false);
        let _ = SetForegroundWindow(window);
        let _ = SetFocus(handle(view(|v| v.edit)));
        switch_keyboard(view(|v| v.lang));
    }
}

pub fn hide() {
    if is_open() {
        unsafe {
            let _ = ShowWindow(handle(view(|v| v.hwnd)), SW_HIDE);
        }
    }
}

/// The list changed (a word was learned): redraw if the window is open.
pub fn refresh() {
    if is_open() {
        reload_items();
    }
}

fn ensure_window() -> Option<HWND> {
    let raw = view(|v| v.hwnd);
    if raw != 0 {
        return Some(handle(raw));
    }
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(words_proc),
            hInstance: instance.into(),
            lpszClassName: w!("TypeFlowWords"),
            hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("TypeFlowWords"),
            w!("TypeFlow: Custom words"),
            STYLE,
            0, 0, 10, 10,
            None, None, instance, None,
        )
        .ok()?;
        for (kind, size) in [(ICON_SMALL as usize, GetSystemMetrics(SM_CXSMICON)), (ICON_BIG as usize, GetSystemMetrics(SM_CXICON))] {
            let icon = tray::icon_of_size(true, size);
            SendMessageW(window, WM_SETICON, WPARAM(kind), LPARAM(icon.0 as isize));
        }
        view(|v| v.hwnd = window.0 as isize);
        let edit = make_edit(window, view(|v| v.lang))?;
        view(|v| v.edit = edit.0 as isize);
        Some(window)
    }
}

unsafe fn set_theme(window: HWND, theme: Theme) {
    view(|v| v.theme = theme);
    crate::theme::apply(window, &theme);
    let c = palette(&theme);
    let brush = CreateSolidBrush(color(c.hover));
    let old = EDIT_BRUSH.with(|b| b.replace(brush.0 as isize));
    if old != 0 {
        let _ = DeleteObject(HBRUSH(old as *mut c_void));
    }
}

fn color(c: (u8, u8, u8)) -> COLORREF {
    COLORREF(c.0 as u32 | (c.1 as u32) << 8 | (c.2 as u32) << 16)
}

// ------------------------------------------------------------------ text box ---
unsafe fn make_edit(parent: HWND, lang: Lang) -> Option<HWND> {
    let instance = GetModuleHandleW(None).ok()?;
    let ex = if lang == Lang::Fa { WS_EX_RIGHT | WS_EX_RTLREADING } else { WINDOW_EX_STYLE(0) };
    let edit = CreateWindowExW(
        ex,
        w!("EDIT"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL_STYLE),
        0, 0, 10, 10,
        parent, None, instance, None,
    )
    .ok()?;
    SendMessageW(edit, EM_SETLIMITTEXT_MSG, WPARAM(MAX_INPUT), LPARAM(0));
    let previous = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
    EDIT_PROC.with(|p| p.set(previous));
    Some(edit)
}

/// Puts the text box inside its field, vertically centered, in the language's font.
unsafe fn place_edit() {
    let (window, edit, dpi, lang) = view(|v| (handle(v.hwnd), handle(v.edit), v.dpi, v.lang));
    let (w, h) = client_size(window);
    let field = layout(w, h, dpi).field;
    let font = with_fonts(dpi, |f| if lang == Lang::Fa { f.fa_body } else { f.body });
    SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    let inset = px(10, dpi);
    let line = px(22, dpi);
    let y = field.top + (field.bottom - field.top - line) / 2;
    let _ = SetWindowPos(edit, None, field.left + inset, y, field.right - field.left - 2 * inset, line, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn edit_text(edit: HWND) -> String {
    let mut buf = [0u16; MAX_INPUT + 1];
    let n = GetWindowTextW(edit, &mut buf).max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe extern "system" fn edit_proc(edit: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_KEYDOWN => match VIRTUAL_KEY(wparam.0 as u16) {
            VK_RETURN => {
                add_word();
                return LRESULT(0);
            }
            VK_ESCAPE => {
                hide();
                return LRESULT(0);
            }
            VK_TAB => {
                let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                move_focus(next_focus(Focus::Edit, back, has_items()));
                return LRESULT(0);
            }
            _ => {}
        },
        // Enter, Esc and Tab would otherwise beep
        WM_CHAR if matches!(wparam.0, 0x0D | 0x1B | 0x09) => return LRESULT(0),
        WM_SETFOCUS => {
            view(|v| v.focus = Focus::Edit);
            redraw();
        }
        WM_KILLFOCUS => redraw(),
        _ => {}
    }
    let previous: WNDPROC = std::mem::transmute(EDIT_PROC.with(Cell::get));
    CallWindowProcW(previous, edit, msg, wparam, lparam)
}

// ------------------------------------------------------------------- actions ---
fn has_items() -> bool {
    view(|v| !v.items.is_empty())
}

fn next_focus(current: Focus, back: bool, has_list: bool) -> Focus {
    let order: &[Focus] = if has_list { &[Focus::Edit, Focus::Language, Focus::List] } else { &[Focus::Edit, Focus::Language] };
    let at = order.iter().position(|f| *f == current).unwrap_or(0);
    let n = order.len();
    order[if back { (at + n - 1) % n } else { (at + 1) % n }]
}

fn move_focus(focus: Focus) {
    let (window, edit) = view(|v| {
        v.focus = focus;
        if focus == Focus::List && v.selected.is_none() && !v.items.is_empty() {
            v.selected = Some(v.top);
        }
        (handle(v.hwnd), handle(v.edit))
    });
    unsafe {
        let _ = SetFocus(if focus == Focus::Edit { edit } else { window });
    }
    redraw();
}

fn redraw() {
    let raw = view(|v| v.hwnd);
    if raw != 0 {
        unsafe {
            let _ = InvalidateRect(handle(raw), None, false);
        }
    }
}

fn set_status(status: String, error: bool) {
    view(|v| {
        v.status = status;
        v.status_error = error;
    });
    redraw();
}

/// Keeps the selection valid and on screen.
fn settle_list(window: HWND) {
    let (w, h) = client_size(window);
    view(|v| {
        let rows = visible_rows(&layout(w, h, v.dpi).list, v.dpi);
        let n = v.items.len();
        v.selected = v.selected.filter(|_| n > 0).map(|s| s.min(n - 1));
        if let Some(s) = v.selected {
            if s < v.top {
                v.top = s;
            } else if s >= v.top + rows {
                v.top = s + 1 - rows;
            }
        }
        v.top = v.top.min(n.saturating_sub(rows));
    });
}

fn reload_items() {
    let items = userwords::entries();
    let window = view(|v| {
        v.items = items;
        v.hover = None;
        v.pressed = None;
        handle(v.hwnd)
    });
    settle_list(window);
    redraw();
}

fn error_text(error: WordError, lang: Lang) -> &'static str {
    match (error, lang) {
        (WordError::Empty, _) => "Type a word first.",
        (WordError::TooShort, _) => "A word needs at least 2 letters.",
        (WordError::TooLong, _) => "That word is too long.",
        (WordError::WrongLanguage, Lang::En) => "Use English letters, or choose فارسی above.",
        (WordError::WrongLanguage, Lang::Fa) => "Use Persian letters, or choose English above.",
        (WordError::NotAWord, Lang::En) => "One word only: letters and an apostrophe.",
        (WordError::NotAWord, Lang::Fa) => "One word only: Persian letters and a half-space.",
        (WordError::AlreadyAdded, _) => "That word is already in your list.",
    }
}

fn add_word() {
    let (edit, lang, window) = view(|v| (handle(v.edit), v.lang, handle(v.hwnd)));
    let input = unsafe { edit_text(edit) };
    match userwords::add(lang, &input) {
        Ok(word) => {
            let saved = userwords::save();
            unsafe {
                let _ = SetWindowTextW(edit, w!(""));
            }
            let items = userwords::entries();
            let at = items.iter().position(|e| e.lang == lang && e.word == word);
            view(|v| {
                v.items = items;
                v.selected = at;
            });
            settle_list(window);
            if saved {
                set_status(format!("Added “{word}”"), false);
            } else {
                set_status("Added for now, but it couldn't be saved.".to_string(), true);
            }
            panel::refresh();
        }
        Err(error) => set_status(error_text(error, lang).to_string(), true),
    }
}

fn remove_item(index: usize) {
    let Some(entry) = view(|v| v.items.get(index).cloned()) else { return };
    userwords::remove(entry.lang, &entry.word);
    let saved = userwords::save();
    reload_items();
    if saved {
        set_status(format!("Removed “{}”", entry.word), false);
    } else {
        set_status("Removed for now, but it couldn't be saved.".to_string(), true);
    }
    panel::refresh();
}

unsafe fn switch_keyboard(lang: Lang) {
    if let Some(hkl) = crate::win::layout_for(lang) {
        let window = handle(view(|v| v.hwnd));
        let _ = PostMessageW(window, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl.0 as isize));
    }
}

/// The text box is rebuilt because its reading direction is fixed when created.
unsafe fn set_language(lang: Lang, focus: Focus) {
    let (window, old, changed) = view(|v| {
        let changed = v.lang != lang;
        v.lang = lang;
        (handle(v.hwnd), handle(v.edit), changed)
    });
    if changed {
        let typed = edit_text(old);
        let Some(edit) = make_edit(window, lang) else { return };
        let wide: Vec<u16> = typed.encode_utf16().chain([0]).collect();
        let _ = SetWindowTextW(edit, PCWSTR(wide.as_ptr()));
        view(|v| v.edit = edit.0 as isize);
        let _ = DestroyWindow(old);
        place_edit();
    }
    switch_keyboard(lang);
    set_status(String::new(), false);
    move_focus(focus);
}

fn activate(hit: Hit) {
    match hit {
        Hit::Language(lang) => unsafe { set_language(lang, Focus::Edit) },
        Hit::Add => {
            add_word();
            move_focus(Focus::Edit);
        }
        Hit::Row(i) => {
            view(|v| v.selected = Some(i));
            move_focus(Focus::List);
        }
        Hit::Remove(i) => remove_item(i),
    }
}

// -------------------------------------------------------------- window proc ---
unsafe extern "system" fn words_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let mouse = || ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    let hit = || {
        let (x, y) = mouse();
        let (w, h) = client_size(window);
        hit_at(x, y, w, h)
    };
    match msg {
        WM_PAINT => paint(window),
        WM_ERASEBKGND => return LRESULT(1),
        WM_CTLCOLOREDIT => {
            let dc = HDC(wparam.0 as *mut c_void);
            let c = palette(&view(|v| v.theme));
            SetTextColor(dc, color(c.text));
            SetBkColor(dc, color(c.hover));
            return LRESULT(EDIT_BRUSH.with(Cell::get));
        }
        WM_SETFOCUS => {
            if view(|v| v.focus) == Focus::Edit {
                let _ = SetFocus(handle(view(|v| v.edit)));
            }
        }
        WM_MOUSEMOVE => {
            if !view(|v| v.tracking) {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
                view(|v| v.tracking = true);
            }
            let now = hit();
            if view(|v| v.hover != now) {
                view(|v| v.hover = now);
                redraw();
            }
        }
        WM_MOUSELEAVE_MSG => {
            view(|v| {
                v.tracking = false;
                v.hover = None;
                v.pressed = None;
            });
            redraw();
        }
        WM_LBUTTONDOWN => {
            let now = hit();
            view(|v| v.pressed = now);
            redraw();
        }
        WM_LBUTTONUP => {
            let now = hit();
            let pressed = view(|v| v.pressed.take());
            redraw();
            if let Some(h) = now.filter(|_| now == pressed) {
                activate(h);
            }
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam.0 >> 16) as u16 as i16;
            let (w, h) = client_size(window);
            view(|v| {
                let rows = visible_rows(&layout(w, h, v.dpi).list, v.dpi);
                let last = v.items.len().saturating_sub(rows);
                v.top = if delta < 0 { (v.top + 2).min(last) } else { v.top.saturating_sub(2) };
            });
            redraw();
        }
        WM_KEYDOWN => on_key(window, VIRTUAL_KEY(wparam.0 as u16)),
        WM_DPICHANGED => {
            view(|v| v.dpi = (wparam.0 & 0xFFFF) as i32);
            let r = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(window, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            place_edit();
            settle_list(window);
            redraw();
        }
        WM_CLOSE => hide(),
        _ => return DefWindowProcW(window, msg, wparam, lparam),
    }
    LRESULT(0)
}

unsafe fn on_key(window: HWND, vk: VIRTUAL_KEY) {
    let focus = view(|v| v.focus);
    match vk {
        VK_ESCAPE => hide(),
        VK_TAB => {
            let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
            move_focus(next_focus(focus, back, has_items()));
        }
        VK_LEFT | VK_RIGHT | VK_SPACE if focus == Focus::Language => {
            let other = view(|v| v.lang.other());
            set_language(other, Focus::Language);
        }
        VK_UP | VK_DOWN if focus == Focus::List => {
            view(|v| {
                let n = v.items.len();
                if n > 0 {
                    let at = v.selected.unwrap_or(0);
                    v.selected = Some(if vk == VK_UP { at.saturating_sub(1) } else { (at + 1).min(n - 1) });
                }
            });
            settle_list(window);
            redraw();
        }
        VK_DELETE if focus == Focus::List => {
            if let Some(i) = view(|v| v.selected) {
                remove_item(i);
                if !has_items() {
                    move_focus(Focus::Edit);
                }
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------- drawing ---
fn draw_shapes(canvas: &mut Canvas, v: &View, c: &Palette, l: &Layout) {
    let f = |n: i32| px(n, v.dpi) as f32;
    let shape = |r: &RECT| (r.left as f32, r.top as f32, (r.right - r.left) as f32, (r.bottom - r.top) as f32);
    canvas.clear(c.bg);

    let (x, y, w, h) = shape(&l.language);
    canvas.round_rect(x, y, w, h, f(6), c.hover, None);
    let (english, persian) = language_halves(&l.language);
    for (rect, lang) in [(english, Lang::En), (persian, Lang::Fa)] {
        let (x, y, w, h) = shape(&rect);
        if v.lang == lang {
            canvas.round_rect(x + f(2), y + f(2), w - f(4), h - f(4), f(5), c.accent, None);
        } else if v.hover == Some(Hit::Language(lang)) {
            canvas.round_rect(x + f(2), y + f(2), w - f(4), h - f(4), f(5), c.pressed, None);
        }
    }
    if v.focus == Focus::Language {
        canvas.round_rect(x, y, w, h, f(6), c.text, Some(f(2).max(1.5)));
    }

    let (x, y, w, h) = shape(&l.field);
    canvas.round_rect(x, y, w, h, f(6), c.hover, None);
    if v.focus == Focus::Edit {
        canvas.round_rect(x, y, w, h, f(6), c.accent, Some(f(2).max(1.5)));
    } else {
        canvas.round_rect(x, y, w, h, f(6), c.line, Some(f(1).max(1.0)));
    }

    let (x, y, w, h) = shape(&l.add);
    canvas.round_rect(x, y, w, h, f(6), c.accent, None);
    if v.hover == Some(Hit::Add) {
        canvas.round_rect(x, y, w, h, f(6), c.on_accent, Some(f(1).max(1.0)));
    }

    let rows = visible_rows(&l.list, v.dpi);
    let row_h = px(ROW, v.dpi);
    for (slot, index) in (v.top..v.items.len().min(v.top + rows)).enumerate() {
        let top = l.list.top + row_h * slot as i32;
        let (x, w) = (l.list.left as f32, (l.list.right - l.list.left) as f32);
        let (y, h) = ((top + px(1, v.dpi)) as f32, (row_h - px(2, v.dpi)) as f32);
        let hovered = matches!(v.hover, Some(Hit::Row(i) | Hit::Remove(i)) if i == index);
        if hovered || v.selected == Some(index) {
            canvas.round_rect(x, y, w, h, f(5), c.hover, None);
        }
        if v.focus == Focus::List && v.selected == Some(index) {
            canvas.round_rect(x, y, w, h, f(5), c.text, Some(f(2).max(1.5)));
        }
        if v.hover == Some(Hit::Remove(index)) {
            let size = f(REMOVE_W - 6);
            canvas.round_rect(l.list.right as f32 - f(REMOVE_W) + f(3), y + (h - size) / 2.0, size, size, f(4), c.pressed, None);
        }
    }
    let n = v.items.len();
    if n > rows {
        let track = (l.list.bottom - l.list.top) as f32;
        let thumb = (track * rows as f32 / n as f32).max(f(20));
        let offset = (track - thumb) * v.top as f32 / (n - rows) as f32;
        canvas.round_rect(l.list.right as f32 + f(5), l.list.top as f32 + offset, f(3), thumb, f(1), c.muted, None);
    }
}

unsafe fn draw_text(dc: HDC, v: &View, c: &Palette, l: &Layout) {
    let n = |value: i32| px(value, v.dpi);
    SetBkMode(dc, TRANSPARENT);
    with_fonts(v.dpi, |fonts| {
        let (english, persian) = language_halves(&l.language);
        let label = |lang: Lang| if v.lang == lang { c.on_accent } else { c.text };
        text(dc, fonts.body, "English", english, label(Lang::En), DT_CENTER);
        text(dc, fonts.fa_body, "فارسی", persian, label(Lang::Fa), DT_CENTER | DT_RTLREADING);
        text(dc, fonts.body, "Add", l.add, c.on_accent, DT_CENTER);

        if !v.status.is_empty() {
            let error = if v.theme.light { (0xC4, 0x2B, 0x1C) } else { (0xFF, 0x99, 0xA4) };
            text(dc, fonts.small, &v.status, l.status, if v.status_error { error } else { c.muted }, DT_LEFT);
        }
        text(dc, fonts.section, "Your words", l.header, c.muted, DT_LEFT);
        text(dc, fonts.small, &v.items.len().to_string(), l.header, c.muted, DT_RIGHT);

        if v.items.is_empty() {
            let r = RECT { bottom: l.list.top + n(ROW), ..l.list };
            text(dc, fonts.small, "No words yet. Add one above.", r, c.muted, DT_LEFT);
            return;
        }
        let rows = visible_rows(&l.list, v.dpi);
        for (slot, entry) in v.items.iter().skip(v.top).take(rows).enumerate() {
            let top = l.list.top + n(ROW) * slot as i32;
            let bottom = top + n(ROW);
            let tag = if entry.lang == Lang::En { "EN" } else { "FA" };
            let tag_rect = RECT { left: l.list.left + n(8), top, right: l.list.left + n(32), bottom };
            text(dc, fonts.small, tag, tag_rect, c.muted, DT_LEFT);

            let mut word_right = l.list.right - n(REMOVE_W) - n(4);
            if entry.learned {
                let learned = RECT { left: word_right - n(LEARNED_W), top, right: word_right, bottom };
                text(dc, fonts.small, "learned", learned, c.muted, DT_RIGHT);
                word_right -= n(LEARNED_W) + n(6);
            }
            let word = RECT { left: l.list.left + n(36), top, right: word_right, bottom };
            if entry.lang == Lang::Fa {
                text(dc, fonts.fa_body, &entry.word, word, c.text, DT_RIGHT | DT_RTLREADING);
            } else {
                text(dc, fonts.body, &entry.word, word, c.text, DT_LEFT);
            }
            let remove = RECT { left: l.list.right - n(REMOVE_W), top, right: l.list.right, bottom };
            text(dc, fonts.body, "×", remove, c.muted, DT_CENTER);
        }
    });
}

unsafe fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(window, &mut ps);
    let (w, h) = client_size(window);
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
        VIEW.with(|cell| {
            let v = cell.borrow();
            let colors = palette(&v.theme);
            let l = layout(w, h, v.dpi);
            draw_shapes(&mut Canvas { px: pixels, w, h }, &v, &colors, &l);
            let _ = GdiFlush();
            draw_text(dc, &v, &colors, &l);
        });
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

    #[test]
    fn tab_order_skips_an_empty_list() {
        assert!(next_focus(Focus::Edit, false, true) == Focus::Language);
        assert!(next_focus(Focus::Language, false, true) == Focus::List);
        assert!(next_focus(Focus::List, false, true) == Focus::Edit);
        assert!(next_focus(Focus::Language, false, false) == Focus::Edit);
        assert!(next_focus(Focus::Edit, true, false) == Focus::Language);
    }

    #[test]
    fn controls_do_not_overlap_at_any_scale() {
        for dpi in [96, 120, 144, 192] {
            let l = layout(px(WIDTH, dpi), px(HEIGHT, dpi), dpi);
            assert!(l.language.bottom <= l.field.top);
            assert!(l.field.right < l.add.left);
            assert!(l.field.bottom <= l.status.top);
            assert!(l.status.bottom <= l.header.top);
            assert!(l.header.bottom <= l.list.top);
            assert!(l.list.bottom <= px(HEIGHT, dpi));
        }
    }

    extern "system" {
        fn PrintWindow(window: HWND, dc: HDC, flags: u32) -> BOOL;
    }

    /// The real window, driven through its messages, with the list stored in a
    /// temporary folder: `cargo test words_window -- --ignored --test-threads=1`.
    /// It shows briefly on screen and leaves target/preview/words_*.bmp.
    #[test]
    #[ignore]
    fn words_window() {
        let folder = std::env::temp_dir().join(format!("typeflow-words-test-{}", std::process::id()));
        std::env::set_var("APPDATA", &folder);
        userwords::clear_for_test();
        crate::win::load_fonts();
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            show();
            let window = handle(view(|v| v.hwnd));
            let enter_word = |word: &str| {
                let edit = handle(view(|v| v.edit));
                let wide: Vec<u16> = word.encode_utf16().chain([0]).collect();
                let _ = SetWindowTextW(edit, PCWSTR(wide.as_ptr()));
                SendMessageW(edit, WM_KEYDOWN, WPARAM(VK_RETURN.0 as usize), LPARAM(0));
            };
            let click = |x: i32, y: i32| {
                let at = LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
                SendMessageW(window, WM_LBUTTONDOWN, WPARAM(1), at);
                SendMessageW(window, WM_LBUTTONUP, WPARAM(0), at);
            };
            let (w, h) = client_size(window);
            let dpi = view(|v| v.dpi);
            let l = layout(w, h, dpi);

            enter_word("سلام");
            assert!(view(|v| v.status_error), "Persian letters while English is selected");
            enter_word("Marmonix");
            assert!(userwords::contains(Lang::En, "marmonix"));

            let (_, persian) = language_halves(&l.language);
            click((persian.left + persian.right) / 2, (persian.top + persian.bottom) / 2);
            assert!(view(|v| v.lang) == Lang::Fa);
            enter_word("تهران\u{200c}پارس");
            assert!(userwords::contains(Lang::Fa, "تهرانپارس"));
            for _ in 0..userwords::LEARN_AFTER {
                userwords::reject(Lang::En, "typeflow");
            }
            refresh();

            let file = folder.join("TypeFlow").join("words.txt");
            let saved = std::fs::read_to_string(&file).unwrap();
            assert!(saved.contains("en\tMarmonix") && saved.contains("fa\tتهران\u{200c}پارس"), "{saved}");

            let capture = |path: &str| {
                let mut frame = RECT::default();
                let _ = GetWindowRect(window, &mut frame);
                let (fw, fh) = (frame.right - frame.left, frame.bottom - frame.top);
                let screen = GetDC(None);
                let dc = CreateCompatibleDC(screen);
                let bitmap = CreateCompatibleBitmap(screen, fw, fh);
                let old = SelectObject(dc, bitmap);
                let _ = PrintWindow(window, dc, 2); // PW_RENDERFULLCONTENT
                SelectObject(dc, old);
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: fw,
                        biHeight: -fh,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut pixels = vec![0u8; (fw * fh * 4) as usize];
                GetDIBits(dc, bitmap, 0, fh as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
                let _ = DeleteObject(bitmap);
                let _ = DeleteDC(dc);
                ReleaseDC(None, screen);
                let mut out = Vec::new();
                out.extend_from_slice(b"BM");
                out.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&54u32.to_le_bytes());
                out.extend_from_slice(&40u32.to_le_bytes());
                out.extend_from_slice(&fw.to_le_bytes());
                out.extend_from_slice(&(-fh).to_le_bytes());
                out.extend_from_slice(&1u16.to_le_bytes());
                out.extend_from_slice(&32u16.to_le_bytes());
                out.extend_from_slice(&[0u8; 24]);
                out.extend_from_slice(&pixels);
                std::fs::create_dir_all("target/preview").unwrap();
                std::fs::write(path, out).unwrap();
            };
            for (light, path) in [(true, "target/preview/words_light.bmp"), (false, "target/preview/words_dark.bmp")] {
                set_theme(window, Theme { light, ..Theme::DEFAULT });
                let _ = InvalidateRect(window, None, false);
                let _ = RedrawWindow(handle(view(|v| v.edit)), None, None, RDW_INVALIDATE | RDW_UPDATENOW);
                let _ = UpdateWindow(window);
                capture(path);
            }

            // the × on the first row
            let count = userwords::count();
            click(l.list.right - px(REMOVE_W, dpi) / 2, l.list.top + px(ROW, dpi) / 2);
            assert_eq!(userwords::count(), count - 1);
            hide();
        }
        userwords::clear_for_test();
        let _ = std::fs::remove_dir_all(&folder);
    }
}
