//! Windows side: low-level keyboard/mouse hooks, key -> character mapping via
//! the user's own installed layouts (`ToUnicodeEx`), the hint bubble, and text
//! replacement via `SendInput`.
//!
//! The overall design (key tracker, "run" of keys with both readings, convert
//! by backspacing and retyping, layout switch with WM_INPUTLANGCHANGEREQUEST,
//! idle check for search boxes) follows ZabanYar's `app.py`
//! (https://github.com/shyguy2018/ZabanYar, MIT, (c) 2026 Behrooz Shayestepoor).
//!
//! Threading: everything runs on the main thread. The hooks only record keys
//! and post messages to our hidden window; all decisions and UI happen in the
//! window procedure. State is never borrowed across a Win32 call that could
//! re-enter the window procedure.

use std::cell::RefCell;
use windows::core::{Interface, VARIANT};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use windows::Win32::System::Threading::CreateMutexW;

use crate::detect::{self, Confidence, Lang};
use crate::mapping;
use crate::settings;
use crate::tray;

// ------------------------------------------------------------------ settings ---
// On/off, auto-fix and sound are runtime settings in the tray menu (tray.rs).

/// Also switch the keyboard layout after converting.
const SWITCH_LAYOUT: bool = true;
/// Pause/Break: accept the hint, or convert the last word manually.
const HOTKEY_VK: u32 = 0x13;
/// Tapping Right Ctrl on its own does the same as Pause (laptops have no Pause key).
const RCTRL_TAP: bool = true;
/// Tab accepts the hint, but only while the bubble is showing.
const TAB_ACCEPTS: bool = true;
/// Check an unfinished word after this pause (search boxes have no Space).
const IDLE_MS: u32 = 1100;
const BUBBLE_MS: u32 = 5000;

const VK_TAB_CODE: u32 = 0x09;
const VK_RCTRL_CODE: u32 = 0xA3;

// ---------------------------------------------------------- private messages ---
const WM_APP_WORD: u32 = WM_APP + 1; // Space pressed
const WM_APP_TYPED: u32 = WM_APP + 2; // a letter was typed
const WM_APP_HOTKEY: u32 = WM_APP + 3;
const WM_APP_RESET: u32 = WM_APP + 4; // click / focus change / arrows...
const WM_APP_ESCAPE: u32 = WM_APP + 5;
const TIMER_IDLE: usize = 1;
const TIMER_HIDE: usize = 2;

// --------------------------------------------------------------------- state ---
struct Key {
    en: String,
    fa: String,
    lang: Lang,
}

impl Key {
    fn text(&self, lang: Lang) -> &str {
        match lang {
            Lang::En => &self.en,
            Lang::Fa => &self.fa,
        }
    }
    fn is_space(&self) -> bool {
        self.en == " "
    }
}

struct Bubble {
    start: usize,
    target: Lang,
    /// Where the bubble is on screen.
    rect: RECT,
    /// The caret it was placed for.
    caret: RECT,
}

struct State {
    window: HWND,
    /// Keys typed since the last reset; spaces are `Key { en: " ", .. }`.
    run: Vec<Key>,
    foreground: HWND,
    en_hkl: Option<HKL>,
    fa_hkl: Option<HKL>,
    /// Increases with every recorded key.
    seq: u64,
    /// Increases whenever `run` is cleared or shifted, so indices into it
    /// carried by posted messages can be checked.
    generation: usize,
    rctrl_down: bool,
    /// Right Ctrl is down and nothing else happened since it was pressed.
    rctrl_alone: bool,
    /// Layout we just switched to; the target app may not have applied it yet.
    pending: Option<(Lang, HWND, Instant)>,
    bubble: Option<Bubble>,
    /// Language the previous word was corrected to, and when.
    last_fix: Option<(Lang, Instant)>,
}

/// What the window procedure must do once the state borrow is released.
enum Action {
    None,
    Show { text: String, target: Lang, start: usize },
    Convert { backspaces: usize, text: String, switch_to: Option<(HKL, HWND)> },
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// What the bubble paints: (suggestion, is_persian).
    static BUBBLE_TEXT: RefCell<(String, bool)> = const { RefCell::new((String::new(), false)) };
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
    /// The hidden window that owns the tray icon.
    static TRAY_WINDOW: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn post(window: HWND, msg: u32, wparam: usize, lparam: usize) {
    unsafe {
        let _ = PostMessageW(window, msg, WPARAM(wparam), LPARAM(lparam as isize));
    }
}

// ------------------------------------------------------------------- helpers ---
fn lang_of(hkl: HKL) -> Option<Lang> {
    match (hkl.0 as usize) & 0xFF {
        0x09 => Some(Lang::En),
        0x29 => Some(Lang::Fa),
        _ => None,
    }
}

fn installed_layouts() -> (Option<HKL>, Option<HKL>) {
    unsafe {
        let n = GetKeyboardLayoutList(None);
        let mut list = vec![HKL::default(); n.max(0) as usize];
        GetKeyboardLayoutList(Some(&mut list));
        let find = |lang| list.iter().copied().find(|h| lang_of(*h) == Some(lang));
        (find(Lang::En), find(Lang::Fa))
    }
}

fn foreground() -> (HWND, u32) {
    unsafe {
        let hwnd = GetForegroundWindow();
        (hwnd, GetWindowThreadProcessId(hwnd, None))
    }
}

fn char_for(vk: u32, scan: u32, shift: bool, caps: bool, hkl: HKL) -> String {
    let mut state = [0u8; 256];
    if shift {
        state[VK_SHIFT.0 as usize] = 0x80;
    }
    if caps {
        state[VK_CAPITAL.0 as usize] = 0x01;
    }
    let mut buf = [0u16; 8];
    // flag 0x4: don't change the keyboard state (no dead-key side effects)
    let n = unsafe { ToUnicodeEx(vk, scan, &state, &mut buf, 0x4, hkl) };
    if n > 0 {
        String::from_utf16_lossy(&buf[..n as usize])
    } else {
        String::new()
    }
}

fn is_typing_vk(vk: u32) -> bool {
    matches!(vk, 0x30..=0x39 | 0x41..=0x5A | 0xBA..=0xC0 | 0xDB..=0xDE)
}

fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10..=0x12 | 0xA0..=0xA5 | 0x14)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

fn send_unicode(text: &str, backspaces: usize) {
    let key = |vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let mut inputs = Vec::new();
    for _ in 0..backspaces {
        inputs.push(key(VK_BACK, 0, KEYBD_EVENT_FLAGS(0)));
        inputs.push(key(VK_BACK, 0, KEYEVENTF_KEYUP));
    }
    for unit in text.encode_utf16() {
        inputs.push(key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
        inputs.push(key(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

// ----------------------------------------------------------------- placement ---

/// Screen rectangle of the text caret (a thin vertical bar), from:
/// 1. the classic Win32 caret (Notepad, Word, most native apps),
/// 2. the accessibility caret, OBJID_CARET (Chrome, Edge, Electron apps such
///    as VS Code and Discord, Firefox),
/// 3. otherwise the mouse pointer, treated as a caret-sized box.
fn caret_rect() -> RECT {
    unsafe {
        let (fg, tid) = foreground();
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let have_info = GetGUIThreadInfo(tid, &mut gti).is_ok();
        if have_info && !gti.hwndCaret.is_invalid() {
            let r = gti.rcCaret;
            let mut top_left = POINT { x: r.left, y: r.top };
            let mut bottom_right = POINT { x: r.right.max(r.left + 1), y: r.bottom };
            let _ = ClientToScreen(gti.hwndCaret, &mut top_left);
            let _ = ClientToScreen(gti.hwndCaret, &mut bottom_right);
            if top_left.x != 0 || top_left.y != 0 {
                return RECT { left: top_left.x, top: top_left.y, right: bottom_right.x, bottom: bottom_right.y };
            }
        }
        let focus = if have_info && !gti.hwndFocus.is_invalid() { gti.hwndFocus } else { fg };
        if let Some(r) = accessible_caret(focus) {
            return r;
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        RECT { left: pt.x, top: pt.y, right: pt.x + 16, bottom: pt.y + 22 }
    }
}

/// The caret as reported through Microsoft Active Accessibility.
unsafe fn accessible_caret(window: HWND) -> Option<RECT> {
    let mut acc: Option<IAccessible> = None;
    AccessibleObjectFromWindow(window, OBJID_CARET.0 as u32, &IAccessible::IID, &mut acc as *mut _ as *mut *mut _).ok()?;
    let acc = acc?;
    let (mut x, mut y, mut w, mut h) = (0, 0, 0, 0);
    acc.accLocation(&mut x, &mut y, &mut w, &mut h, &VARIANT::from(0i32)).ok()?; // CHILDID_SELF
    if h <= 0 || (x == 0 && y == 0) {
        return None;
    }
    Some(RECT { left: x, top: y, right: x + w.max(1), bottom: y + h })
}

/// Work area (screen minus taskbar) of the monitor showing `r`.
fn work_area(r: RECT) -> RECT {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: r.left, y: r.bottom }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) }
        }
    }
}

/// Gap between the caret and the bubble.
const CARET_GAP: i32 = 6;
/// Minimum distance between the bubble and the screen edges / taskbar.
const EDGE_MARGIN: i32 = 8;

/// Where to put a `w` x `h` bubble for a caret at `caret`.
///
/// Preferred: below the caret and "after" it in the direction the text runs
/// (to the right for English, to the left for Persian), so it covers neither
/// the line being typed nor the text already written. If that would put it
/// off-screen or under the taskbar, it flips above the caret and/or to the
/// other side; if neither fits it is pushed inside the edges.
fn place_bubble(caret: RECT, w: i32, h: i32, work: RECT, rtl_text: bool) -> (i32, i32) {
    let (min_x, max_x) = (work.left + EDGE_MARGIN, work.right - EDGE_MARGIN - w);
    let (min_y, max_y) = (work.top + EDGE_MARGIN, work.bottom - EDGE_MARGIN - h);
    let clamp = |v: i32, lo: i32, hi: i32| v.min(hi).max(lo);

    let below = caret.bottom + CARET_GAP;
    let above = caret.top - CARET_GAP - h;
    let y = if below <= max_y {
        below
    } else if above >= min_y {
        above
    } else {
        clamp(below, min_y, max_y)
    };

    let starts_at_caret = caret.left; // extends to the right
    let ends_at_caret = caret.right - w; // extends to the left
    let (first, second) = if rtl_text { (ends_at_caret, starts_at_caret) } else { (starts_at_caret, ends_at_caret) };
    let fits = |x: i32| x >= min_x && x <= max_x;
    let x = if fits(first) {
        first
    } else if fits(second) {
        second
    } else {
        clamp(first, min_x, max_x)
    };
    (x, y)
}

fn last_word_span(run: &[Key], skip_trailing_space: bool) -> (usize, usize) {
    let mut end = run.len();
    if skip_trailing_space && end > 0 && run[end - 1].is_space() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && !run[start - 1].is_space() {
        start -= 1;
    }
    (start, end)
}

// --------------------------------------------------------------------- hooks ---
impl State {
    fn reset(&mut self) {
        self.run.clear();
        self.seq += 1;
        self.generation += 1;
        post(self.window, WM_APP_RESET, 0, 0);
    }

    /// Returns true to swallow the key.
    fn on_key(&mut self, vk: u32, scan: u32) -> bool {
        if vk == VK_RCTRL_CODE {
            if !self.rctrl_down {
                self.rctrl_down = true; // ignore auto-repeat
                self.rctrl_alone = true;
            }
            return false;
        }
        self.rctrl_alone = false;

        let (hwnd, tid) = foreground();
        if hwnd != self.foreground {
            self.foreground = hwnd;
            self.reset();
        }
        if vk == HOTKEY_VK {
            post(self.window, WM_APP_HOTKEY, 0, 0);
            return true;
        }
        let shortcut = key_down(VK_CONTROL) || key_down(VK_MENU) || key_down(VK_LWIN) || key_down(VK_RWIN);
        if TAB_ACCEPTS && vk == VK_TAB_CODE && self.bubble.is_some() && !shortcut {
            post(self.window, WM_APP_HOTKEY, 0, 0);
            return true;
        }
        if is_modifier_vk(vk) {
            return false;
        }
        if shortcut {
            self.reset(); // shortcuts (Ctrl+V, Alt+Tab...) make our copy of the text unreliable
            return false;
        }

        let mut lang = lang_of(unsafe { GetKeyboardLayout(tid) });
        if let Some((pending, window, until)) = self.pending {
            if lang == Some(pending) || hwnd != window || Instant::now() > until {
                self.pending = None;
            } else {
                lang = Some(pending);
            }
        }

        match vk {
            0x08 /* Backspace */ => {
                self.run.pop();
                self.seq += 1;
                return false;
            }
            0x20 /* Space */ => {
                let lang = lang.unwrap_or(Lang::En);
                self.run.push(Key { en: " ".into(), fa: " ".into(), lang });
                self.seq += 1;
                // pass where this word ends: by the time the message is handled,
                // a fast typist may already be in the middle of the next word
                post(self.window, WM_APP_WORD, self.run.len(), self.generation);
                return false;
            }
            0x1B /* Esc */ => {
                post(self.window, WM_APP_ESCAPE, 0, 0);
                return false;
            }
            _ => {}
        }
        let Some(lang) = lang.filter(|_| is_typing_vk(vk)) else {
            self.reset(); // arrows, Enter, Tab, Home, or a third layout
            return false;
        };

        let shift = key_down(VK_SHIFT);
        let caps = unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
        if self.en_hkl.is_none() || self.fa_hkl.is_none() {
            (self.en_hkl, self.fa_hkl) = installed_layouts();
        }
        let mut en = self.en_hkl.map(|h| char_for(vk, scan, shift, caps, h)).unwrap_or_default();
        let mut fa = self.fa_hkl.map(|h| char_for(vk, scan, shift, caps, h)).unwrap_or_default();
        if en.is_empty() && !fa.is_empty() {
            en = mapping::fa_to_en(&fa);
        }
        if fa.is_empty() {
            fa = mapping::en_to_fa(&en);
        }
        if en.is_empty() || fa.is_empty() {
            self.reset();
            return false;
        }
        self.run.push(Key { en, fa, lang });
        if self.run.len() > 400 {
            self.run.drain(..200);
            self.generation += 1;
            self.bubble = None;
        }
        self.seq += 1;
        post(self.window, WM_APP_TYPED, self.seq as usize, 0);
        false
    }

    fn on_key_up(&mut self, vk: u32) {
        if vk == VK_RCTRL_CODE {
            if RCTRL_TAP && self.rctrl_alone {
                post(self.window, WM_APP_HOTKEY, 0, 0);
            }
            self.rctrl_down = false;
            self.rctrl_alone = false;
        }
    }

    fn on_click(&mut self, pt: POINT) {
        self.rctrl_alone = false; // Ctrl+click is not a tap
        if let Some(b) = &self.bubble {
            let r = b.rect;
            if (r.left..=r.right).contains(&pt.x) && (r.top..=r.bottom).contains(&pt.y) {
                return; // a click on our own bubble keeps the text
            }
        }
        self.reset();
    }

    // -------------------------------------------------------------- decisions ---

    /// Checks the word ending at `run[..end]` together with up to
    /// PHRASE_WORDS - 1 words before it. Called on Space (`idle == false`,
    /// `end` just after the space) or after a typing pause (`idle == true`,
    /// `end` = everything typed so far).
    fn evaluate(&mut self, end: usize, idle: bool) -> Action {
        let end = end.min(self.run.len());

        // collect word spans, newest first, all typed with the same layout
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut typed: Option<Lang> = None;
        let mut pos = end;
        let mut skip_space = !idle;
        while spans.len() < detect::PHRASE_WORDS {
            let (s, e) = last_word_span(&self.run[..pos], skip_space);
            skip_space = true;
            if s == e {
                break;
            }
            let lang = self.run[s].lang;
            if self.run[s..e].iter().any(|k| k.lang != lang) || typed.is_some_and(|t| t != lang) {
                break;
            }
            typed = Some(lang);
            spans.push((s, e));
            pos = s;
        }
        let Some(typed) = typed else { return Action::None };
        if idle && spans[0].1 - spans[0].0 < 2 {
            return Action::None;
        }
        spans.reverse();

        let readings: Vec<(String, String)> = spans
            .iter()
            .map(|&(s, e)| {
                let keys = &self.run[s..e];
                (keys.iter().map(|k| k.en.as_str()).collect(), keys.iter().map(|k| k.fa.as_str()).collect())
            })
            .collect();
        let refs: Vec<(&str, &str)> = readings.iter().map(|(e, f)| (e.as_str(), f.as_str())).collect();
        let context = self
            .last_fix
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(30))
            .map(|(lang, _)| lang);

        let Some(verdict) = detect::decide_phrase(&refs, typed, context) else {
            if !idle {
                self.last_fix = None;
            }
            return Action::None;
        };
        let target = verdict.target;
        let mut start = spans[verdict.first].0;
        self.last_fix = Some((target, Instant::now()));
        println!(
            "hint: {} word(s) {:?} -> {:?} ({:?})",
            spans.len() - verdict.first,
            typed,
            target,
            verdict.confidence
        );

        if tray::get(&tray::AUTO_FIX) && verdict.confidence == Confidence::Sure {
            return self.convert_from(start, target);
        }

        // an earlier word in the same direction is still waiting: extend the hint
        if let Some(b) = &self.bubble {
            if b.target == target && b.start < start {
                start = b.start;
            }
        }
        let wrong = target.other();
        let text: String = self.run[start..end]
            .iter()
            .map(|k| k.text(if k.lang == wrong { target } else { k.lang }))
            .collect::<String>()
            .trim()
            .to_string();
        let text = if target == Lang::Fa { mapping::normalize_fa(&text) } else { text };
        Action::Show { text, target, start }
    }

    /// Retype everything from `run[start..]` so keys typed in the wrong layout
    /// become `target` (keys already in `target` stay as they are).
    fn convert_from(&mut self, start: usize, target: Lang) -> Action {
        if start >= self.run.len() {
            return Action::None;
        }
        let wrong = target.other();
        let tail = &mut self.run[start..];
        let text: String = tail.iter().map(|k| k.text(if k.lang == wrong { target } else { k.lang })).collect();
        let text = if target == Lang::Fa { mapping::normalize_fa(&text) } else { text };
        for k in tail.iter_mut() {
            k.lang = target;
        }
        let backspaces = tail.len();
        self.bubble = None;

        let hkl = match target {
            Lang::En => self.en_hkl,
            Lang::Fa => self.fa_hkl,
        };
        let switch_to = hkl.filter(|_| SWITCH_LAYOUT).map(|h| {
            self.pending = Some((target, self.foreground, Instant::now() + Duration::from_secs(2)));
            (h, self.foreground)
        });
        Action::Convert { backspaces, text, switch_to }
    }

    fn on_hotkey(&mut self) -> Action {
        if let Some(b) = &self.bubble {
            let (start, target) = (b.start, b.target);
            return self.convert_from(start, target);
        }
        let (start, end) = last_word_span(&self.run, true);
        if start == end {
            return Action::None;
        }
        let target = self.run[start].lang.other();
        self.convert_from(start, target)
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam.0 as u32;
    if code == HC_ACTION as i32 && tray::get(&tray::ENABLED) {
        let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        // ignore keys we inject ourselves with SendInput
        if k.flags.0 & LLKHF_INJECTED.0 == 0 {
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                if with_state(|s| s.on_key(k.vkCode, k.scanCode)) == Some(true) {
                    return LRESULT(1);
                }
            } else if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                with_state(|s| s.on_key_up(k.vkCode));
            }
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam.0 as u32;
    if code == HC_ACTION as i32 && tray::get(&tray::ENABLED) && (msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN) {
        let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        with_state(|s| s.on_click(m.pt));
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

// -------------------------------------------------------------------- bubble ---
const BG: COLORREF = COLORREF(0x0031_2D2B); // 0x00BBGGRR
const FG: COLORREF = COLORREF(0x00FF_FFFF);
const MUTED: COLORREF = COLORREF(0x00B0_A8A0);
const PAD: i32 = 12;
/// Space between the two labels on the first row.
const GAP: i32 = 24;
const LABEL_FA: &str = "منظورتون این بود؟";
const LABEL_EN: &str = "Tab ✓   Esc ×";

// Fonts bundled into the exe and registered privately for this process
// (see assets/fonts/ and THIRD_PARTY_LICENSES.md).
const FONT_FILES: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/UbuntuSansCondensed-Light.ttf"),
    include_bytes!("../assets/fonts/Vazirmatn-RD-Light.ttf"),
];
pub const EN_FONT: PCWSTR = w!("Ubuntu Sans Condensed Light");
pub const FA_FONT: PCWSTR = w!("Vazirmatn RD Light");

fn load_fonts() {
    for data in FONT_FILES {
        let mut count = 0u32;
        unsafe {
            AddFontMemResourceEx(data.as_ptr() as *const _, data.len() as u32, None, &mut count);
        }
    }
}

struct Fonts {
    label_fa: HFONT,
    label_en: HFONT,
    main_fa: HFONT,
    main_en: HFONT,
}

impl Fonts {
    fn main(&self, rtl: bool) -> HFONT {
        if rtl { self.main_fa } else { self.main_en }
    }
}

/// "New Notification 07" by Universfield, trimmed and converted to 16-bit
/// mono WAV (see assets/sounds/ and THIRD_PARTY_LICENSES.md).
static DING_WAV: &[u8] = include_bytes!("../assets/sounds/notification.wav");

fn ding() {
    if tray::get(&tray::DING) {
        unsafe {
            let _ = PlaySoundW(PCWSTR(DING_WAV.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

fn with_fonts<R>(f: impl FnOnce(&Fonts) -> R) -> R {
    FONTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let fonts = slot.get_or_insert_with(|| unsafe {
            let dpi = GetDpiForSystem() as i32;
            let make = |pt: i32, face: PCWSTR| {
                CreateFontW(-(pt * dpi / 72), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET.0 as u32,
                    OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, face)
            };
            Fonts {
                label_fa: make(10, FA_FONT),
                label_en: make(10, EN_FONT),
                main_fa: make(17, FA_FONT),
                main_en: make(18, EN_FONT),
            }
        });
        f(fonts)
    })
}

fn text_size(hdc: HDC, font: HFONT, s: &str, rtl: bool) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe { text_line(hdc, font, s, &mut r, DT_CALCRECT | line_flags(rtl)) };
    (r.right, r.bottom)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Draws (or with DT_CALCRECT, measures) one line of text.
unsafe fn text_line(hdc: HDC, font: HFONT, s: &str, rect: &mut RECT, flags: DRAW_TEXT_FORMAT) {
    SelectObject(hdc, font);
    let mut buf = wide(s);
    DrawTextW(hdc, &mut buf, rect, flags | DT_SINGLELINE | DT_NOPREFIX);
}

fn line_flags(rtl: bool) -> DRAW_TEXT_FORMAT {
    if rtl {
        DT_RTLREADING | DT_RIGHT
    } else {
        DT_LEFT
    }
}

fn show_bubble(window: HWND, text: &str, target: Lang, start: usize) {
    let rtl = target == Lang::Fa;
    BUBBLE_TEXT.with(|b| *b.borrow_mut() = (text.to_string(), rtl));
    unsafe {
        // row 1 = "Tab ✓ Esc ×" (left) + Persian label (right), row 2 = suggestion
        let hdc = GetDC(window);
        let (width, height) = measure_bubble(hdc, text, rtl);
        ReleaseDC(window, hdc);
        // A growing bubble keeps the caret position it first appeared at, so
        // it doesn't jump around; only its size changes.
        let existing = with_state(|s| s.bubble.as_ref().map(|b| b.caret)).flatten();
        let caret = existing.unwrap_or_else(caret_rect);
        // the text on screen is in the layout that was typed, i.e. not `target`
        let rtl_text = target == Lang::En;
        let (x, y) = place_bubble(caret, width, height, work_area(caret), rtl_text);
        if existing.is_none() {
            ding();
        }
        let _ = SetWindowPos(window, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetWindowRgn(window, CreateRoundRectRgn(0, 0, width + 1, height + 1, 12, 12), true);
        let _ = InvalidateRect(window, None, true);
        SetTimer(window, TIMER_HIDE, BUBBLE_MS, None);

        let rect = RECT { left: x, top: y, right: x + width, bottom: y + height };
        with_state(|s| s.bubble = Some(Bubble { start, target, rect, caret }));
    }
}

fn hide_bubble(window: HWND) {
    with_state(|s| s.bubble = None);
    unsafe {
        let _ = KillTimer(window, TIMER_HIDE);
        let _ = ShowWindow(window, SW_HIDE);
    }
}

unsafe fn paint(window: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(window, &mut ps);
    let mut client = RECT::default();
    let _ = GetClientRect(window, &mut client);
    let (text, rtl) = BUBBLE_TEXT.with(|b| b.borrow().clone());
    draw_bubble(hdc, client, &text, rtl);
    let _ = EndPaint(window, &ps);
}

/// Size the bubble needs for `text`.
unsafe fn measure_bubble(hdc: HDC, text: &str, rtl: bool) -> (i32, i32) {
    with_fonts(|f| {
        let (fa_w, fa_h) = text_size(hdc, f.label_fa, LABEL_FA, true);
        let (en_w, en_h) = text_size(hdc, f.label_en, LABEL_EN, false);
        let (main_w, main_h) = text_size(hdc, f.main(rtl), text, rtl);
        ((en_w + GAP + fa_w).max(main_w) + 2 * PAD, fa_h.max(en_h) + main_h + 2 * PAD + 2)
    })
}

unsafe fn draw_bubble(hdc: HDC, client: RECT, text: &str, rtl: bool) {
    let brush = CreateSolidBrush(BG);
    FillRect(hdc, &client, brush);
    let _ = DeleteObject(brush);

    SetBkMode(hdc, TRANSPARENT);
    let inner = RECT { left: PAD, top: PAD, right: client.right - PAD, bottom: client.bottom - PAD };

    with_fonts(|f| {
        SetTextColor(hdc, MUTED);
        let mut r = inner;
        text_line(hdc, f.label_en, LABEL_EN, &mut r, DT_LEFT | DT_TOP);
        let mut r = inner;
        text_line(hdc, f.label_fa, LABEL_FA, &mut r, DT_RTLREADING | DT_RIGHT | DT_TOP);

        SetTextColor(hdc, FG);
        let mut r = inner;
        text_line(hdc, f.main(rtl), text, &mut r, line_flags(rtl) | DT_BOTTOM);
    });
}

// -------------------------------------------------------------- window proc ---
fn perform(window: HWND, action: Action) {
    match action {
        Action::None => {}
        Action::Show { text, target, start } => show_bubble(window, &text, target, start),
        Action::Convert { backspaces, text, switch_to } => {
            hide_bubble(window);
            send_unicode(&text, backspaces);
            if let Some((hkl, target_window)) = switch_to {
                unsafe {
                    let _ = PostMessageW(target_window, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl.0 as isize));
                }
            }
        }
    }
}

fn bubble_window() -> Option<HWND> {
    with_state(|s| s.window)
}

/// A tray menu command or a left click on the icon.
fn tray_command(tray_window: HWND, cmd: usize) {
    match cmd {
        tray::CMD_ENABLED => {
            let on = tray::toggle(&tray::ENABLED);
            with_state(|s| {
                s.run.clear();
                s.generation += 1;
            });
            if !on {
                if let Some(w) = bubble_window() {
                    hide_bubble(w);
                }
            }
            tray::update(tray_window);
        }
        tray::CMD_AUTO_FIX => {
            tray::toggle(&tray::AUTO_FIX);
        }
        tray::CMD_DING => {
            if tray::toggle(&tray::DING) {
                ding(); // let the user hear what they turned on
            }
        }
        tray::CMD_AUTOSTART => sync_autostart(tray::toggle(&tray::START_WITH_WINDOWS)),
        tray::CMD_EXIT => unsafe {
            tray::remove(tray_window);
            PostQuitMessage(0);
            return;
        },
        _ => return,
    }
    settings::save(&tray::current());
}

/// Registers/unregisters the running exe to start at sign-in. Debug builds
/// leave the registry alone so it never points at target\debug.
fn sync_autostart(on: bool) {
    if cfg!(debug_assertions) {
        println!("(debug build: Start with Windows is saved as {on} but the registry is not changed)");
    } else {
        settings::autostart::set(on);
    }
}

unsafe extern "system" fn window_proc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let is_tray_window = TRAY_WINDOW.with(|t| t.get()) == window.0 as isize;
    if is_tray_window {
        if msg == tray::WM_TRAY {
            match (lparam.0 as u32) & 0xFFFF {
                WM_LBUTTONUP => tray_command(window, tray::CMD_ENABLED),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let cmd = tray::show_menu(window);
                    tray_command(window, cmd);
                }
                _ => {}
            }
            return LRESULT(0);
        }
        if msg != 0 && msg == TASKBAR_CREATED.with(|t| t.get()) {
            tray::add(window); // Explorer restarted: put the icon back
            return LRESULT(0);
        }
        return DefWindowProcW(window, msg, wparam, lparam);
    }

    match msg {
        WM_APP_WORD => {
            let _ = KillTimer(window, TIMER_IDLE);
            let (end, generation) = (wparam.0, lparam.0 as usize);
            let action = with_state(|s| {
                // the text was cleared or shifted since that Space: indices are stale
                if s.generation == generation { s.evaluate(end, false) } else { Action::None }
            });
            perform(window, action.unwrap_or(Action::None));
        }
        WM_APP_TYPED => {
            SetTimer(window, TIMER_IDLE, IDLE_MS, None); // restarts on every key
        }
        WM_TIMER if wparam.0 == TIMER_IDLE => {
            let _ = KillTimer(window, TIMER_IDLE);
            let action = with_state(|s| {
                let finished = s.run.last().map_or(true, |k| k.is_space());
                if finished { Action::None } else { s.evaluate(s.run.len(), true) }
            });
            perform(window, action.unwrap_or(Action::None));
        }
        WM_TIMER if wparam.0 == TIMER_HIDE => hide_bubble(window),
        WM_APP_HOTKEY | WM_LBUTTONDOWN => {
            let action = with_state(|s| s.on_hotkey()).unwrap_or(Action::None);
            perform(window, action);
        }
        WM_APP_RESET => {
            with_state(|s| s.last_fix = None);
            hide_bubble(window);
        }
        WM_APP_ESCAPE => hide_bubble(window),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => paint(window),
        _ => return DefWindowProcW(window, msg, wparam, lparam),
    }
    LRESULT(0)
}

// ---------------------------------------------------------------------- run ---
pub fn run() -> Result<()> {
    unsafe {
        // only one copy at a time: two keyboard hooks would fight each other
        let _mutex = CreateMutexW(None, true, w!("Local\\TypeFlow-single-instance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            MessageBoxW(None, w!("TypeFlow is already running. Look for its icon near the clock."),
                w!("TypeFlow"), MB_OK | MB_ICONINFORMATION);
            return Ok(());
        }

        // saved settings (defaults on first run, written back so the file exists)
        let saved = settings::load();
        tray::apply(&saved);
        settings::save(&saved);
        sync_autostart(saved.start_with_windows); // also refreshes the exe path

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        load_fonts();
        // COM is needed for the accessibility caret lookup (Chrome, Electron)
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("TypeFlowBubble"),
            hCursor: LoadCursorW(None, IDC_HAND)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("TypeFlowBubble"),
            PCWSTR::null(),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, instance, None,
        )?;
        // A separate, never-shown window owns the tray icon: the bubble window
        // can't be activated, which the tray menu needs to close properly.
        let tray_window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("TypeFlowBubble"),
            w!("TypeFlow"),
            WS_POPUP,
            0, 0, 0, 0,
            None, None, instance, None,
        )?;
        TRAY_WINDOW.with(|t| t.set(tray_window.0 as isize));
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
        tray::add(tray_window);

        let (en_hkl, fa_hkl) = installed_layouts();
        if en_hkl.is_none() || fa_hkl.is_none() {
            println!("warning: install both the English and Persian keyboards in Windows settings.");
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                window,
                run: Vec::new(),
                foreground: HWND::default(),
                en_hkl,
                fa_hkl,
                seq: 0,
                generation: 0,
                rctrl_down: false,
                rctrl_alone: false,
                pending: None,
                bubble: None,
                last_fix: None,
            })
        });

        let _kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0)?;
        let _mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0)?;

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State {
            window: HWND::default(),
            run: Vec::new(),
            foreground: HWND::default(),
            en_hkl: None,
            fa_hkl: None,
            seq: 0,
            generation: 0,
            rctrl_down: false,
            rctrl_alone: false,
            pending: None,
            bubble: None,
            last_fix: None,
        }
    }

    /// Simulates typing `s` with the English layout active.
    fn type_en(st: &mut State, s: &str) {
        for ch in s.chars() {
            let c = ch.to_string();
            st.run.push(Key { en: c.clone(), fa: mapping::en_to_fa(&c), lang: Lang::En });
        }
    }

    fn shown(a: Action) -> Option<String> {
        match a {
            Action::Show { text, .. } => Some(text),
            _ => None,
        }
    }

    #[test]
    fn checks_the_word_before_the_space_even_if_typing_continued() {
        let mut st = state();
        type_en(&mut st, "sghl ");
        let end = st.run.len(); // what the Space message carries
        type_en(&mut st, "o,"); // typed before the message was handled
        assert_eq!(shown(st.evaluate(end, false)).as_deref(), Some("سلام"));
    }

    #[test]
    fn whole_phrase_is_suggested() {
        let mut st = state();
        type_en(&mut st, "di v, n, fh ");
        let end = st.run.len();
        assert_eq!(shown(st.evaluate(end, false)).as_deref(), Some("یه رو دو با"));
    }

    #[test]
    fn idle_check_on_unfinished_word() {
        let mut st = state();
        type_en(&mut st, "sghl");
        let end = st.run.len();
        assert_eq!(shown(st.evaluate(end, true)).as_deref(), Some("سلام"));
    }

    // ------------------------------------------------------------ placement ---
    const SCREEN: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1040 }; // taskbar below 1040
    const W: i32 = 300;
    const H: i32 = 80;

    fn caret(x: i32, y: i32) -> RECT {
        RECT { left: x, top: y, right: x + 1, bottom: y + 20 }
    }

    #[test]
    fn english_text_bubble_goes_below_and_to_the_right() {
        assert_eq!(place_bubble(caret(500, 300), W, H, SCREEN, false), (500, 326));
    }

    #[test]
    fn persian_text_bubble_goes_below_and_to_the_left() {
        assert_eq!(place_bubble(caret(1000, 300), W, H, SCREEN, true), (1001 - W, 326));
    }

    #[test]
    fn flips_above_near_the_taskbar() {
        // e.g. a chat box at the bottom of the screen
        let (_, y) = place_bubble(caret(500, 990), W, H, SCREEN, false);
        assert_eq!(y, 990 - CARET_GAP - H);
    }

    #[test]
    fn flips_sideways_near_the_right_and_left_edges() {
        let (x, _) = place_bubble(caret(1800, 300), W, H, SCREEN, false);
        assert_eq!(x, 1801 - W); // English near the right edge: extend left instead
        let (x, _) = place_bubble(caret(100, 300), W, H, SCREEN, true);
        assert_eq!(x, 100); // Persian near the left edge: extend right instead
    }

    #[test]
    fn stays_inside_when_nothing_fits() {
        let (x, y) = place_bubble(caret(960, 300), 1910, H, SCREEN, false);
        assert_eq!(x, EDGE_MARGIN); // too wide for either side: pinned to the left margin
        assert_eq!(y, 326);
    }

    #[test]
    fn respects_a_second_monitor() {
        let right_monitor = RECT { left: 1920, top: 0, right: 3840, bottom: 1040 };
        // Persian near that monitor's left edge: extends right, not onto the other monitor
        assert_eq!(place_bubble(caret(1950, 300), W, H, right_monitor, true), (1950, 326));
    }

    #[test]
    fn ding_is_a_valid_wav() {
        let wav = DING_WAV;
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize, wav.len() - 8);
    }
}

// ------------------------------------------------------------------ previews ---
/// Renders the bubble and the tray icons to target/preview/*.bmp so they can be
/// looked at without running the app: `cargo test previews -- --ignored`.
#[cfg(test)]
mod previews {
    use super::*;

    fn save_bmp(path: &str, w: i32, h: i32, pixels: &[u32]) {
        let mut out = Vec::new();
        let size = 54 + pixels.len() as u32 * 4;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&(-h).to_le_bytes()); // top-down
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 24]);
        for p in pixels {
            out.extend_from_slice(&p.to_le_bytes());
        }
        std::fs::create_dir_all("target/preview").unwrap();
        std::fs::write(path, out).unwrap();
    }

    unsafe fn render_bubble(text: &str, rtl: bool) -> (i32, i32, Vec<u32>) {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let (w, h) = measure_bubble(dc, text, rtl);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(dc, bmp);
        draw_bubble(dc, RECT { left: 0, top: 0, right: w, bottom: h }, text, rtl);
        SelectObject(dc, old);
        let mut info = BITMAPINFO {
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
        let mut px = vec![0u32; (w * h) as usize];
        GetDIBits(dc, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        (w, h, px)
    }

    /// Nearest-neighbour upscale, composited on white.
    fn upscale(size: i32, px: &[u32], k: i32) -> Vec<u32> {
        let mut out = Vec::new();
        for y in 0..size * k {
            for x in 0..size * k {
                let p = px[((y / k) * size + x / k) as usize];
                let a = (p >> 24) as u32;
                let ch = |s: u32| ((p >> s) & 0xFF) + (255 - a); // premultiplied over white
                out.push(ch(16).min(255) << 16 | ch(8).min(255) << 8 | ch(0).min(255));
            }
        }
        out
    }

    #[test]
    #[ignore]
    fn previews() {
        load_fonts();
        unsafe {
            let (w, h, px) = render_bubble("سلام خوبی؟", true);
            save_bmp("target/preview/bubble_fa.bmp", w, h, &px);
            let (w, h, px) = render_bubble("you mean hello", false);
            save_bmp("target/preview/bubble_en.bmp", w, h, &px);
        }
        for (name, on) in [("icon_on", true), ("icon_off", false)] {
            for size in [16, 24, 32] {
                let px = tray::render_icon(size, on);
                save_bmp(&format!("target/preview/{name}_{size}.bmp"), size * 8, size * 8, &upscale(size, &px, 8));
            }
        }
    }
}
