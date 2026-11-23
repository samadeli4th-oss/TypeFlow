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
use crate::fields;
use crate::mapping;
use crate::panel;
use crate::settings::{self, FieldAction};
use crate::spell::{self, Suggestion};
use crate::suggest;
use crate::theme;
use crate::tray;
use crate::userwords;
use crate::words;

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
/// How long after an auto-fix Pause, Right Ctrl or Ctrl+Z still counts as undoing it.
const UNDO_WINDOW: Duration = Duration::from_secs(20);

const VK_TAB_CODE: u32 = 0x09;
const VK_RCTRL_CODE: u32 = 0xA3;

// ---------------------------------------------------------- private messages ---
const WM_APP_WORD: u32 = WM_APP + 1; // Space pressed
const WM_APP_TYPED: u32 = WM_APP + 2; // a letter was typed
const WM_APP_HOTKEY: u32 = WM_APP + 3;
const WM_APP_RESET: u32 = WM_APP + 4; // click / focus change / arrows...
const WM_APP_ESCAPE: u32 = WM_APP + 5;
const WM_APP_SPELL: u32 = WM_APP + 6; // the spelling thread has a reply
pub(crate) const WM_APP_SPELL_PICK: u32 = WM_APP + 7; // a row was clicked (wparam)
pub(crate) const WM_APP_SPELL_HOVER: u32 = WM_APP + 8; // the mouse is on a row (wparam)
const WM_APP_SPELL_ACCEPT: u32 = WM_APP + 9; // Tab, or Space after arrows (wparam 1)
const WM_APP_SPELL_MOVE: u32 = WM_APP + 10; // Up (wparam 1) / Down (wparam 0)
const WM_APP_FIELD: u32 = WM_APP + 11; // focus moved (wparam = fields::FLAG_*)
const WM_APP_UNDO: u32 = WM_APP + 12; // Ctrl+Z right after an auto-fix
const TIMER_IDLE: usize = 1;
const TIMER_HIDE: usize = 2;
const TIMER_SPELL: usize = 3;

// --------------------------------------------------------------------- state ---
struct Key {
    en: String,
    fa: String,
    lang: Lang,
    sep: bool,
    /// The user rejected converting this word: later hints leave it out.
    kept: bool,
}

impl Key {
    fn text(&self, lang: Lang) -> &str {
        match lang {
            Lang::En => &self.en,
            Lang::Fa => &self.fa,
        }
    }
    fn new(en: String, fa: String, lang: Lang) -> Key {
        // Punctuation and spaces separate words. Keys that are a letter in
        // either layout (like `,` -> و or `;` -> ک) belong to the word.
        // (the half-space U+200C and combining marks belong to Persian words too)
        let sep = !en.chars().chain(fa.chars()).any(|c| {
            c.is_alphanumeric() || matches!(c, '\u{200c}' | '\u{200d}' | '\u{0300}'..='\u{036f}' | '\u{064b}'..='\u{065f}')
        });
        Key { en, fa, lang, sep, kept: false }
    }
    fn is_space(&self) -> bool {
        self.en == " "
    }
    fn is_sep(&self) -> bool {
        self.sep
    }
}

struct Bubble {
    start: usize,
    target: Lang,
    /// Where the bubble is on screen.
    rect: RECT,
    /// The caret it was placed for.
    caret: RECT,
    /// The word this hint is about, when it covers exactly one.
    candidate: Option<Candidate>,
}

/// A single word that a hint or auto-fix wanted to convert: `run[start..end]`,
/// as typed. Rejecting it enough times makes it a valid word.
#[derive(Clone)]
struct Candidate {
    start: usize,
    end: usize,
    generation: usize,
    lang: Lang,
    word: String,
}

/// The newest auto-fix, until something is typed after it.
struct AutoFix {
    start: usize,
    seq: u64,
    generation: usize,
    at: Instant,
    /// The layout the text was typed in.
    typed: Lang,
    candidate: Option<Candidate>,
}

/// A finished word being spell-checked: `run[start..end]` reads `word`.
struct SpellTarget {
    start: usize,
    end: usize,
    generation: usize,
    lang: Lang,
    word: String,
}

/// The suggestion list on screen.
struct SpellChoice {
    target: SpellTarget,
    items: Vec<Suggestion>,
    selected: usize,
    rect: RECT,
    /// The arrow keys were used, and nothing else typed since: Space accepts.
    navigated: bool,
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
    /// The focused control, to notice focus moving inside a window.
    control: isize,
    /// Keys of the current word went unrecorded: leave it alone until Space.
    skip_word: bool,
    /// The suggestion list, while it is showing.
    spell: Option<SpellChoice>,
    /// The newest word sent to the spelling thread.
    spell_pending: Option<(u64, SpellTarget)>,
    spell_service: Option<spell::Service>,
    spell_next: u64,
    /// End of the last Shift+Space. It may have typed a half-space, which the
    /// run records as a space, so text up to here can't be safely retyped.
    spell_barrier: usize,
    auto_fix: Option<AutoFix>,
    /// An auto-fix undone with Ctrl+Z, for the window procedure to finish.
    undone: Option<AutoFix>,
}

/// What the window procedure must do once the state borrow is released.
enum Action {
    None,
    Show { text: String, target: Lang, start: usize, candidate: Option<Candidate> },
    Convert { backspaces: usize, text: String, switch_to: Option<(HKL, HWND)> },
    ShowSpell { items: Vec<Suggestion>, rtl: bool },
    SelectSpell(usize),
    HideSpell,
    /// Apply a spelling fix: no layout switch.
    Replace { backspaces: usize, text: String },
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// What the bubble paints: (suggestion, is_persian).
    static BUBBLE_TEXT: RefCell<(String, bool, theme::Theme)> = const { RefCell::new((String::new(), false, theme::Theme::DEFAULT)) };
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
    /// The hidden window that owns the tray icon.
    static TRAY_WINDOW: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Sent by the installer to close a running copy before updating or removing it.
    static EXIT_REQUEST: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
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

/// Thread whose keyboard layout is in effect: the one owning the focused
/// control. Apps like Outlook host the editor on another thread than the
/// top-level window, which can report a stale layout.
fn layout_thread(fg_tid: u32) -> u32 {
    unsafe {
        let mut gti = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(fg_tid, &mut gti).is_ok() && gti.hwndFocus.0 as usize != 0 {
            let t = GetWindowThreadProcessId(gti.hwndFocus, None);
            if t != 0 {
                return t;
            }
        }
        fg_tid
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
pub(crate) fn place_bubble(caret: RECT, w: i32, h: i32, work: RECT, rtl_text: bool) -> (i32, i32) {
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

fn last_word_span(run: &[Key], skip_trailing_seps: bool) -> (usize, usize) {
    let mut end = run.len();
    if skip_trailing_seps {
        while end > 0 && run[end - 1].is_sep() {
            end -= 1;
        }
    }
    let mut start = end;
    while start > 0 && !run[start - 1].is_sep() {
        start -= 1;
    }
    (start, end)
}

/// Widen a word start to include punctuation glued to its front, e.g. the
/// `(` in `(word`, so it is converted along with the word.
fn with_leading_punct(run: &[Key], mut start: usize) -> usize {
    let lang = run.get(start).map(|k| k.lang);
    while start > 0 && run[start - 1].is_sep() && !run[start - 1].is_space() && Some(run[start - 1].lang) == lang {
        start -= 1;
    }
    start
}

// --------------------------------------------------------------------- hooks ---
impl State {
    fn reset(&mut self) {
        self.skip_word = false;
        self.forget_text();
    }

    /// Clears everything held from what was typed and closes the popups.
    fn forget_text(&mut self) {
        self.run.clear();
        self.spell_barrier = 0;
        self.spell_pending = None;
        self.auto_fix = None;
        self.seq += 1;
        self.generation += 1;
        post(self.window, WM_APP_RESET, 0, 0);
    }

    /// A key in a field not confirmed safe: it is not looked at, and text
    /// from before it is dropped because focus may have moved.
    fn drop_key(&mut self) {
        self.skip_word = true;
        self.undone = None;
        if !self.run.is_empty() || self.bubble.is_some() || self.spell.is_some() || self.auto_fix.is_some() {
            self.forget_text();
        }
    }

    /// Returns true to swallow the key.
    fn on_key(&mut self, vk: u32, scan: u32) -> bool {
        // The gate: nothing past it runs for a key unless the field is confirmed safe.
        let (hwnd, tid) = foreground();
        let control = fields::focused_control(tid);
        if hwnd != self.foreground || control != self.control {
            self.foreground = hwnd;
            self.control = control;
            // the watcher has usually confirmed the new control by now
            if !fields::is_safe_for(control) {
                fields::poke();
            }
            self.reset();
        }
        if panel::is_panel(hwnd) || words::owns(hwnd) {
            self.rctrl_alone = false;
            return false; // keys in TypeFlow's own windows are not typing
        }
        if !fields::is_safe_for(control) {
            self.rctrl_alone = false;
            self.drop_key();
            return false;
        }

        if vk == VK_RCTRL_CODE {
            if !self.rctrl_down {
                self.rctrl_down = true; // ignore auto-repeat
                self.rctrl_alone = true;
            }
            return false;
        }
        self.rctrl_alone = false;
        if vk == HOTKEY_VK && tray::get(&tray::ENABLED) {
            post(self.window, WM_APP_HOTKEY, 0, 0);
            return true;
        }
        let shortcut = key_down(VK_CONTROL) || key_down(VK_MENU) || key_down(VK_LWIN) || key_down(VK_RWIN);
        if TAB_ACCEPTS && vk == VK_TAB_CODE && self.bubble.is_some() && !shortcut {
            post(self.window, WM_APP_HOTKEY, 0, 0);
            return true;
        }
        // the suggestion list: a layout hint always comes first (above)
        if self.spell.is_some() && !shortcut {
            if vk == VK_TAB_CODE {
                post(self.window, WM_APP_SPELL_ACCEPT, 0, 0);
                return true;
            }
            if (vk == VK_UP.0 as u32 || vk == VK_DOWN.0 as u32) && !key_down(VK_SHIFT) {
                post(self.window, WM_APP_SPELL_MOVE, (vk == VK_UP.0 as u32) as usize, 0);
                return true;
            }
            if vk == 0x20 && !key_down(VK_SHIFT) && self.spell.as_ref().is_some_and(|c| c.navigated) {
                post(self.window, WM_APP_SPELL_ACCEPT, 1, 0);
                return true;
            }
        }
        if is_modifier_vk(vk) {
            return false;
        }
        // typing on after using the arrows: Space is an ordinary space again
        if let Some(c) = &mut self.spell {
            c.navigated = false;
        }
        if shortcut {
            if vk == 0x5A && key_down(VK_CONTROL) && !key_down(VK_SHIFT) && !key_down(VK_MENU) {
                // no file access in the hook: the window procedure finishes this
                if let Some(fix) = self.fresh_auto_fix() {
                    self.undone = Some(fix);
                    post(self.window, WM_APP_UNDO, 0, 0);
                }
            } else {
                fields::poke(); // any other shortcut can move focus
            }
            self.reset(); // shortcuts (Ctrl+V, Alt+Tab...) make our copy of the text unreliable
            return false;
        }

        let mut lang = lang_of(unsafe { GetKeyboardLayout(layout_thread(tid)) });
        if let Some((pending, window, until)) = self.pending {
            if lang == Some(pending) || hwnd != window || Instant::now() > until {
                self.pending = None;
            } else {
                lang = Some(pending);
            }
        }

        // A PIN, one-time code or card number can sit in a plain text box.
        // Digits are never converted anyway, so they are never recorded.
        if (0x30..=0x39).contains(&vk) && !key_down(VK_SHIFT) {
            self.skip_word = true;
            if !self.run.is_empty() {
                self.forget_text();
            }
            return false;
        }
        // part of this word went unrecorded, so it can't be judged or retyped
        if self.skip_word {
            if vk == 0x20 {
                self.skip_word = false;
                return false;
            }
            if is_typing_vk(vk) {
                return false;
            }
        }

        match vk {
            0x08 /* Backspace */ => {
                self.run.pop();
                self.spell_barrier = self.spell_barrier.min(self.run.len());
                self.seq += 1;
                return false;
            }
            0x20 /* Space */ => {
                let lang = lang.unwrap_or(Lang::En);
                self.run.push(Key::new(" ".into(), " ".into(), lang));
                if key_down(VK_SHIFT) {
                    self.spell_barrier = self.run.len();
                }
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
            if vk == VK_TAB_CODE || vk == VK_RETURN.0 as u32 {
                fields::poke(); // these often move focus to the next field
            }
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
        let is_sep = {
            let k = Key::new(en.clone(), fa.clone(), lang);
            let sep = k.is_sep();
            self.run.push(k);
            sep
        };
        if self.run.len() > 400 {
            self.run.drain(..200);
            self.spell_barrier = self.spell_barrier.saturating_sub(200);
            self.generation += 1;
            self.bubble = None;
        }
        self.seq += 1;
        if is_sep {
            // punctuation ends a word just like Space does
            post(self.window, WM_APP_WORD, self.run.len(), self.generation);
        } else {
            post(self.window, WM_APP_TYPED, self.seq as usize, 0);
        }
        false
    }

    fn on_key_up(&mut self, vk: u32) {
        if vk == VK_RCTRL_CODE {
            if RCTRL_TAP && self.rctrl_alone && tray::get(&tray::ENABLED) && fields::is_safe() {
                post(self.window, WM_APP_HOTKEY, 0, 0);
            }
            self.rctrl_down = false;
            self.rctrl_alone = false;
        }
    }

    /// `pt` is on the hint or the suggestion list. They never take focus.
    fn on_own_popup(&self, pt: POINT) -> bool {
        let on = |r: RECT| (r.left..r.right).contains(&pt.x) && (r.top..r.bottom).contains(&pt.y);
        self.bubble.as_ref().is_some_and(|b| on(b.rect)) || self.spell.as_ref().is_some_and(|c| on(c.rect))
    }

    fn on_click(&mut self, pt: POINT) {
        self.rctrl_alone = false; // Ctrl+click is not a tap
        if !self.on_own_popup(pt) {
            self.reset();
        }
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
            if self.run[s..e].iter().any(|k| k.lang != lang || k.kept) || typed.is_some_and(|t| t != lang) {
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
        let mut start = with_leading_punct(&self.run, spans[verdict.first].0);
        self.last_fix = Some((target, Instant::now()));
        println!(
            "hint: {} word(s) {:?} -> {:?} ({:?})",
            spans.len() - verdict.first,
            typed,
            target,
            verdict.confidence
        );

        // only a verdict on one word says anything about that word
        let (word_start, word_end) = spans[spans.len() - 1];
        let mut candidate = (verdict.first + 1 == spans.len()).then(|| Candidate {
            start: word_start,
            end: word_end,
            generation: self.generation,
            lang: typed,
            word: self.text_of(word_start, word_end),
        });

        if tray::get(&tray::AUTO_FIX) && verdict.confidence == Confidence::Sure {
            let action = self.convert_from(start, target);
            self.auto_fix = Some(AutoFix {
                start,
                seq: self.seq,
                generation: self.generation,
                at: Instant::now(),
                typed,
                candidate,
            });
            return action;
        }

        // an earlier word in the same direction is still waiting: extend the hint
        if let Some(b) = &self.bubble {
            if b.target == target && b.start < start {
                start = b.start;
                candidate = None;
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
        Action::Show { text, target, start, candidate }
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

        let switch_to = self.layout_switch(target);
        Action::Convert { backspaces, text, switch_to }
    }

    /// The layout to switch to, remembered as pending until the app applies it.
    fn layout_switch(&mut self, target: Lang) -> Option<(HKL, HWND)> {
        let hkl = match target {
            Lang::En => self.en_hkl,
            Lang::Fa => self.fa_hkl,
        };
        hkl.filter(|_| SWITCH_LAYOUT).map(|h| {
            self.pending = Some((target, self.foreground, Instant::now() + Duration::from_secs(2)));
            (h, self.foreground)
        })
    }

    /// The hotkey or a click on the hint. Also returns a word the user just
    /// rejected by undoing its auto-fix.
    fn on_hotkey(&mut self) -> (Action, Option<Candidate>) {
        if let Some(b) = &self.bubble {
            let (start, target) = (b.start, b.target);
            if let Some(c) = &b.candidate {
                userwords::accepted(c.lang, &c.word);
            }
            return (self.convert_from(start, target), None);
        }
        if let Some(fix) = self.fresh_auto_fix() {
            self.last_fix = None;
            let action = self.convert_from(fix.start, fix.typed);
            for k in &mut self.run[fix.start..] {
                k.kept = true;
            }
            return (action, fix.candidate);
        }
        let (start, end) = last_word_span(&self.run, true);
        if start == end {
            return (Action::None, None);
        }
        let target = self.run[start].lang.other();
        (self.convert_from(with_leading_punct(&self.run, start), target), None)
    }

    /// The last auto-fix, if nothing has been typed since and it's recent.
    fn fresh_auto_fix(&mut self) -> Option<AutoFix> {
        let fix = self.auto_fix.take()?;
        (fix.seq == self.seq && fix.generation == self.generation && fix.at.elapsed() < UNDO_WINDOW).then_some(fix)
    }

    /// The word of a hint that is going away unused. With `typed_on` it only
    /// counts if the user carried on typing after it, since a hint that merely
    /// timed out may never have been seen. A word the user went back and
    /// changed doesn't count: that means the hint was right.
    fn rejected_hint(&mut self, typed_on: bool) -> Option<Candidate> {
        let c = self.bubble.as_ref()?.candidate.clone()?;
        let intact = c.generation == self.generation
            && c.end <= self.run.len()
            && self.text_of(c.start, c.end) == c.word
            && self.run.get(c.end).map_or(true, Key::is_sep);
        if !intact || (typed_on && self.run[c.end..].iter().all(Key::is_sep)) {
            return None;
        }
        for k in &mut self.run[c.start..c.end] {
            k.kept = true;
        }
        Some(c)
    }

    // --------------------------------------------------------------- spelling ---

    fn text_of(&self, start: usize, end: usize) -> String {
        self.run[start..end].iter().map(|k| k.text(k.lang)).collect()
    }

    /// The word finished by the Space at `run[end - 1]`, with what its keys
    /// spell in the other layout. Only plain words qualify: anything glued to
    /// other symbols (URLs, emails, paths, code), numbers, ALL CAPS, mixedCase,
    /// and capitalized names in mid-sentence are left alone.
    fn spell_target(&self, end: usize) -> Option<(SpellTarget, String)> {
        let end = end.min(self.run.len());
        if end == 0 || !self.run[end - 1].is_space() {
            return None;
        }
        let token_end = end - 1;
        let mut token_start = token_end;
        while token_start > 0 && !self.run[token_start - 1].is_space() {
            token_start -= 1;
        }
        if token_start == token_end || (self.spell_barrier > token_start && self.spell_barrier <= end) {
            return None;
        }
        let keys = &self.run[token_start..token_end];
        let lang = keys[0].lang;
        if keys.iter().any(|k| k.lang != lang || k.text(lang).chars().count() != 1) {
            return None;
        }
        let chars: Vec<char> = keys.iter().filter_map(|k| k.text(lang).chars().next()).collect();
        let (mut a, mut b) = (0, chars.len());
        while a < b && "\"'([{«“‘".contains(chars[a]) {
            a += 1;
        }
        while b > a && "\"')]}»”’.,!?:;،؛؟…".contains(chars[b - 1]) {
            b -= 1;
        }
        let word = &chars[a..b];
        let letters = word.iter().filter(|c| c.is_alphabetic()).count();
        if letters < spell::MIN_LETTERS {
            return None;
        }
        match lang {
            Lang::En => {
                let edge = |c: &char| c.is_ascii_alphabetic();
                if !word.iter().all(|c| c.is_ascii_alphabetic() || *c == '\'')
                    || !word.first().is_some_and(edge)
                    || !word.last().is_some_and(edge)
                    || word[1..].iter().any(char::is_ascii_uppercase)
                {
                    return None;
                }
                if word[0].is_ascii_uppercase() {
                    let before = self.run[..token_start].iter().rev().find(|k| !k.is_space());
                    let sentence_start =
                        before.map_or(true, |k| k.text(k.lang).chars().last().is_some_and(|c| ".!?؟:".contains(c)));
                    if !sentence_start {
                        return None;
                    }
                }
            }
            Lang::Fa => {
                if !word.iter().all(|c| mapping::is_persian_char(*c)) {
                    return None;
                }
            }
        }
        let other: String = keys[a..b].iter().map(|k| k.text(lang.other())).collect();
        let target = SpellTarget {
            start: token_start + a,
            end: token_start + b,
            generation: self.generation,
            lang,
            word: word.iter().collect(),
        };
        Some((target, other))
    }

    /// Sends the word just finished to the spelling thread.
    fn request_spelling(&mut self, end: usize) {
        if self.bubble.is_some() || !fields::is_safe() {
            return; // a layout hint always comes first
        }
        let Some((target, other)) = self.spell_target(end) else { return };
        if !spell_enabled(target.lang) || userwords::contains(target.lang, &detect::lookup_key(target.lang, &target.word)) {
            return;
        }
        let Some(service) = &self.spell_service else { return };
        self.spell_next += 1;
        service.check(self.spell_next, target.lang, target.word.clone(), other);
        self.spell_pending = Some((self.spell_next, target));
    }

    /// The word is still exactly where it was when it was checked.
    fn is_current(&self, t: &SpellTarget) -> bool {
        t.generation == self.generation && t.end <= self.run.len() && self.text_of(t.start, t.end) == t.word
    }

    fn on_spell_reply(&mut self) -> Action {
        let mut action = Action::None;
        while let Some(reply) = self.spell_service.as_ref().and_then(|s| s.next_reply()) {
            if self.spell_pending.as_ref().map(|p| p.0) != Some(reply.id) {
                continue; // a newer word has been typed since
            }
            let Some((_, target)) = self.spell_pending.take() else { continue };
            if reply.suggestions.is_empty()
                || self.bubble.is_some()
                || !fields::is_safe()
                || !spell_enabled(target.lang)
                || !self.is_current(&target)
            {
                continue;
            }
            let rtl = target.lang == Lang::Fa;
            let items = reply.suggestions;
            self.spell = Some(SpellChoice { target, items: items.clone(), selected: 0, rect: RECT::default(), navigated: false });
            action = Action::ShowSpell { items, rtl };
        }
        action
    }

    fn move_spell(&mut self, up: bool) -> Action {
        let Some(c) = &mut self.spell else { return Action::None };
        let n = c.items.len();
        c.selected = if up { (c.selected + n - 1) % n } else { (c.selected + 1) % n };
        c.navigated = true;
        Action::SelectSpell(c.selected)
    }

    fn hover_spell(&mut self, row: usize) -> Action {
        match &mut self.spell {
            Some(c) if row < c.items.len() => {
                c.selected = row;
                Action::SelectSpell(row)
            }
            _ => Action::None,
        }
    }

    /// Replaces the word with the selected suggestion and retypes whatever
    /// was typed after it. With `add_space`, the text ends in exactly one
    /// space: the one that finished the word counts.
    fn accept_spell(&mut self, add_space: bool) -> Action {
        let Some(choice) = self.spell.take() else { return Action::None };
        let t = &choice.target;
        if !self.is_current(t) || self.spell_barrier > t.end {
            return Action::HideSpell;
        }
        let tail = &self.run[t.end..];
        if tail.iter().any(|k| k.text(k.lang).chars().count() != 1) {
            return Action::HideSpell;
        }
        let tail_text: String = tail.iter().map(|k| k.text(k.lang)).collect();
        let replacement = &choice.items[choice.selected].word;
        let backspaces = self.run.len() - t.start;
        let keys: Vec<Key> = replacement.chars().map(|c| spelled_key(c, t.lang)).collect();
        let mut text = format!("{replacement}{tail_text}");
        self.run.splice(t.start..t.end, keys);
        if add_space && !tail_text.ends_with(' ') {
            text.push(' ');
            self.run.push(Key::new(" ".into(), " ".into(), t.lang));
        }
        self.generation += 1; // indices after the word have moved
        self.seq += 1;
        Action::Replace { backspaces, text }
    }
}

fn spell_enabled(lang: Lang) -> bool {
    tray::get(match lang {
        Lang::En => &tray::AUTOCORRECT_EN,
        Lang::Fa => &tray::AUTOCORRECT_FA,
    })
}

/// Counts a rejected hint or auto-fix, if learning is on.
fn learn_rejection(candidate: Option<Candidate>) {
    let Some(c) = candidate else { return };
    if tray::get(&tray::LEARN_WORDS) && fields::is_safe() && userwords::reject(c.lang, &c.word) {
        userwords::save();
        words::refresh();
        panel::refresh();
    }
}

/// The installed keyboard layout for `lang`.
pub(crate) fn layout_for(lang: Lang) -> Option<HKL> {
    with_state(|s| {
        if s.en_hkl.is_none() || s.fa_hkl.is_none() {
            (s.en_hkl, s.fa_hkl) = installed_layouts();
        }
        match lang {
            Lang::En => s.en_hkl,
            Lang::Fa => s.fa_hkl,
        }
    })
    .flatten()
}

/// A key for a corrected letter, so later layout conversions still work.
fn spelled_key(c: char, lang: Lang) -> Key {
    let s = c.to_string();
    match lang {
        Lang::En => Key::new(s.clone(), mapping::en_to_fa(&s), lang),
        Lang::Fa => Key::new(mapping::fa_to_en(&s), s, lang),
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam.0 as u32;
    if code == HC_ACTION as i32 && tray::active() {
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

/// Focus just entered a password or number field. If the keyboard isn't
/// English, play the sound and, if set to, switch it.
fn on_field_entered() {
    let action = tray::field_action();
    if action == FieldAction::Disable {
        return;
    }
    let (window, thread) = foreground();
    if panel::is_panel(window) || words::owns(window) || lang_of(unsafe { GetKeyboardLayout(layout_thread(thread)) }) == Some(Lang::En) {
        return;
    }
    play_now(FIELD_WAV);
    if action != FieldAction::English {
        return;
    }
    let english = with_state(|s| {
        if s.en_hkl.is_none() {
            (s.en_hkl, s.fa_hkl) = installed_layouts();
        }
        if s.en_hkl.is_some() {
            s.pending = Some((Lang::En, window, Instant::now() + Duration::from_secs(2)));
        }
        s.en_hkl
    })
    .flatten();
    if let Some(hkl) = english {
        unsafe {
            let _ = PostMessageW(window, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl.0 as isize));
        }
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam.0 as u32;
    if code == HC_ACTION as i32 && (msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN) {
        let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if !with_state(|s| s.on_own_popup(m.pt)).unwrap_or(false) {
            fields::poke(); // a click may have moved focus
        }
    }
    if code == HC_ACTION as i32 && tray::active() && (msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN) {
        let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        with_state(|s| s.on_click(m.pt));
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

// -------------------------------------------------------------------- bubble ---
// A one-line glass pill holding only the suggestion; colors follow Windows
// (theme.rs). Sizes are at 96 DPI.
const HEIGHT: i32 = 28;
const PAD: i32 = 12;
/// Longer phrases end in "…".
const MAX_WIDTH: i32 = 420;
/// 10 pt Persian, 11 pt English (96-DPI pixels).
const FA_PX: i32 = 13;
const EN_PX: i32 = 15;

// The Persian font is bundled into the exe and registered privately for this
// process (see assets/fonts/ and THIRD_PARTY_LICENSES.md). English uses Segoe UI.
const FONT_FILES: [&[u8]; 1] = [include_bytes!("../assets/fonts/Vazirmatn-RD-Light.ttf")];
const FA_FONT: PCWSTR = w!("Vazirmatn RD Light");
const EN_FONT: PCWSTR = w!("Segoe UI");

pub(crate) fn load_fonts() {
    for data in FONT_FILES {
        let mut count = 0u32;
        unsafe {
            AddFontMemResourceEx(data.as_ptr() as *const _, data.len() as u32, None, &mut count);
        }
    }
}

struct Fonts {
    fa: HFONT,
    en: HFONT,
    dpi: i32,
}

impl Fonts {
    fn main(&self, rtl: bool) -> HFONT {
        if rtl { self.fa } else { self.en }
    }

    /// 96-DPI pixels to device pixels.
    fn px(&self, v: i32) -> i32 {
        v * self.dpi / 96
    }
}

/// Played when the keyboard layout looks wrong (see THIRD_PARTY_LICENSES.md).
static LAYOUT_WAV: &[u8] = include_bytes!("../assets/sounds/layout.wav");
/// Played when the suggestion list opens for a typo.
static TYPO_WAV: &[u8] = include_bytes!("../assets/sounds/typo.wav");

/// Played when a password or number field opens with a non-English keyboard.
/// It has its own setting, so the Sound switch doesn't apply to it.
static FIELD_WAV: &[u8] = include_bytes!("../assets/sounds/password-layout.wav");

/// Plays a sound unless Sound is off.
fn play(sound: &'static [u8]) {
    if tray::get(&tray::DING) {
        play_now(sound);
    }
}

/// The data must outlive the playback.
fn play_now(sound: &'static [u8]) {
    unsafe {
        let _ = PlaySoundW(PCWSTR(sound.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

fn with_fonts<R>(f: impl FnOnce(&Fonts) -> R) -> R {
    FONTS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let fonts = slot.get_or_insert_with(|| unsafe {
            let dpi = GetDpiForSystem() as i32;
            // ClearType: the bubble is solid, so sub-pixel text is fine
            let make = |px: i32, face: PCWSTR| {
                CreateFontW(-(px * dpi / 96), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET.0 as u32,
                    OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32, 0, face)
            };
            Fonts { fa: make(FA_PX, FA_FONT), en: make(EN_PX, EN_FONT), dpi }
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

fn show_bubble(window: HWND, text: &str, target: Lang, start: usize, candidate: Option<Candidate>) {
    let rtl = target == Lang::Fa;
    let theme = theme::Theme::current();
    BUBBLE_TEXT.with(|b| *b.borrow_mut() = (text.to_string(), rtl, theme));
    theme::apply(window, &theme);
    unsafe {
        let (width, height) = bubble_size(text, rtl);
        // A growing bubble keeps the caret position it first appeared at, so
        // it doesn't jump around; only its size changes.
        let existing = with_state(|s| s.bubble.as_ref().map(|b| b.caret)).flatten();
        let caret = existing.unwrap_or_else(caret_rect);
        // the text on screen is in the layout that was typed, i.e. not `target`
        let rtl_text = target == Lang::En;
        let (x, y) = place_bubble(caret, width, height, work_area(caret), rtl_text);
        if existing.is_none() {
            play(LAYOUT_WAV);
        }
        // corners are rounded by DWM (theme::apply), not a window region,
        // which would clip the glass
        let _ = SetWindowPos(window, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(window, None, false);
        SetTimer(window, TIMER_HIDE, BUBBLE_MS, None);

        let rect = RECT { left: x, top: y, right: x + width, bottom: y + height };
        with_state(|s| s.bubble = Some(Bubble { start, target, rect, caret, candidate }));
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
    let (text, rtl, theme) = BUBBLE_TEXT.with(|b| b.borrow().clone());
    let (w, h, pixels) = render_bubble(&text, rtl, &theme.colors());
    if w == client.right && h == client.bottom {
        SetDIBitsToDevice(hdc, 0, 0, w as u32, h as u32, 0, 0, 0, h as u32,
            pixels.as_ptr() as *const _, &dib_info(w, h), DIB_RGB_COLORS);
    }
    let _ = EndPaint(window, &ps);
}

/// Size the bubble needs for `text`: one 32 px line, at most 420 px wide.
unsafe fn bubble_size(text: &str, rtl: bool) -> (i32, i32) {
    with_fonts(|f| {
        let dc = CreateCompatibleDC(None);
        let (w, _) = text_size(dc, f.main(rtl), text, rtl);
        let _ = DeleteDC(dc);
        ((w + 2 * f.px(PAD)).min(f.px(MAX_WIDTH)), f.px(HEIGHT))
    })
}

fn dib_info(w: i32, h: i32) -> BITMAPINFO {
    BITMAPINFO {
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
    }
}

/// The bubble as premultiplied ARGB pixels. GDI can't draw text with alpha,
/// so the text is drawn white on black, its grey levels are used as coverage,
/// and it is composited here over the fill. With glass on the fill's alpha is
/// 0 in the pixels (the tint is in the acrylic itself, see theme.rs), so DWM
/// shows the blurred backdrop through everything but the letters.
unsafe fn render_bubble(text: &str, rtl: bool, c: &theme::Colors) -> (i32, i32, Vec<u32>) {
    let (w, h) = bubble_size(text, rtl);
    let rgb = |(r, g, b): (u8, u8, u8)| COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16);
    let pixels = with_fonts(|f| {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(dc, bmp);
        let mut rect = RECT { left: 0, top: 0, right: w, bottom: h };
        let brush = CreateSolidBrush(rgb(c.fill));
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(brush);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, rgb(c.text));
        rect.left = f.px(PAD);
        rect.right = w - f.px(PAD);
        text_line(dc, f.main(rtl), text, &mut rect, line_flags(rtl) | DT_VCENTER | DT_END_ELLIPSIS);
        SelectObject(dc, old);
        let mut px = vec![0u32; (w * h) as usize];
        GetDIBits(dc, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut dib_info(w, h), DIB_RGB_COLORS);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        px.iter().map(|p| p | 0xFF00_0000).collect::<Vec<u32>>()
    });
    (w, h, pixels)
}

// -------------------------------------------------------------- window proc ---
fn perform(window: HWND, action: Action) {
    // focus may have moved since this was decided: never show or retype text then
    let uses_text = matches!(
        action,
        Action::Show { .. } | Action::Convert { .. } | Action::ShowSpell { .. } | Action::Replace { .. }
    );
    if uses_text && !fields::is_safe() {
        wipe_text(window);
        return;
    }
    match action {
        Action::None => {}
        Action::Show { text, target, start, candidate } => {
            hide_spell(window); // one popup at a time, and layout comes first
            show_bubble(window, &text, target, start, candidate);
        }
        Action::Convert { backspaces, text, switch_to } => {
            hide_spell(window);
            hide_bubble(window);
            send_unicode(&text, backspaces);
            if let Some((hkl, target_window)) = switch_to {
                unsafe {
                    let _ = PostMessageW(target_window, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl.0 as isize));
                }
            }
        }
        Action::ShowSpell { items, rtl } => {
            let caret = caret_rect();
            let rect = suggest::show(window, &items, 0, rtl, caret, work_area(caret));
            with_state(|s| {
                if let (Some(c), Some(r)) = (&mut s.spell, rect) {
                    c.rect = r;
                }
            });
            play(TYPO_WAV);
            unsafe { SetTimer(window, TIMER_SPELL, BUBBLE_MS, None) };
        }
        Action::SelectSpell(row) => {
            suggest::select(row);
            unsafe { SetTimer(window, TIMER_SPELL, BUBBLE_MS, None) };
        }
        Action::HideSpell => hide_spell(window),
        Action::Replace { backspaces, text } => {
            hide_spell(window);
            send_unicode(&text, backspaces);
        }
    }
}

/// The field is no longer confirmed safe: keep nothing from it.
fn wipe_text(window: HWND) {
    with_state(|s| {
        s.undone = None;
        if !s.run.is_empty() || s.auto_fix.is_some() {
            s.forget_text();
        }
    });
    hide_bubble(window);
    hide_spell(window);
}

fn hide_spell(window: HWND) {
    with_state(|s| s.spell = None);
    unsafe {
        let _ = KillTimer(window, TIMER_SPELL);
    }
    suggest::hide();
}

/// Starts or frees the spelling thread's data to match the two switches.
fn sync_spell() {
    let on = tray::get(&tray::AUTOCORRECT_EN) || tray::get(&tray::AUTOCORRECT_FA);
    with_state(|s| {
        s.run.clear();
        s.spell_barrier = 0;
        s.spell_pending = None;
        s.generation += 1;
        if on {
            let window = s.window;
            s.spell_service.get_or_insert_with(|| spell::Service::start(window, WM_APP_SPELL)).wake();
        } else if let Some(service) = &s.spell_service {
            service.unload();
        }
    });
    if let Some(window) = bubble_window() {
        hide_spell(window);
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
                    hide_spell(w);
                }
            }
            tray::update(tray_window);
        }
        tray::CMD_AUTO_FIX => {
            tray::toggle(&tray::AUTO_FIX);
        }
        tray::CMD_DING => {
            if tray::toggle(&tray::DING) {
                play(LAYOUT_WAV); // let the user hear what they turned on
            }
        }
        tray::CMD_LEARN => {
            if !tray::toggle(&tray::LEARN_WORDS) {
                userwords::forget_rejections();
            }
        }
        tray::CMD_WORDS => {
            panel::hide();
            words::show();
            return;
        }
        tray::CMD_AUTOSTART => sync_autostart(tray::toggle(&tray::START_WITH_WINDOWS)),
        tray::CMD_AUTOCORRECT_EN => {
            tray::toggle(&tray::AUTOCORRECT_EN);
            sync_spell();
        }
        tray::CMD_AUTOCORRECT_FA => {
            tray::toggle(&tray::AUTOCORRECT_FA);
            sync_spell();
        }
        tray::CMD_FIELD_ENGLISH => tray::set_field_action(FieldAction::English),
        tray::CMD_FIELD_NOTIFY => tray::set_field_action(FieldAction::Notify),
        tray::CMD_FIELD_DISABLE => tray::set_field_action(FieldAction::Disable),
        tray::CMD_OPEN => {
            panel::show(tray_window);
            return;
        }
        tray::CMD_EXIT => unsafe {
            panel::hide();
            words::hide();
            tray::remove(tray_window);
            PostQuitMessage(0);
            return;
        },
        _ => return,
    }
    fields::poke(); // the watcher may have been idle while everything was off
    settings::save(&tray::current());
    panel::refresh();
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
                WM_LBUTTONUP => panel::toggle(window),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let cmd = tray::show_menu(window);
                    tray_command(window, cmd);
                }
                _ => {}
            }
            return LRESULT(0);
        }
        if msg == tray::WM_PANEL_COMMAND {
            tray_command(window, wparam.0);
            return LRESULT(0);
        }
        if msg != 0 && msg == EXIT_REQUEST.with(|t| t.get()) {
            tray_command(window, tray::CMD_EXIT);
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
            if !fields::is_safe() {
                wipe_text(window);
                return LRESULT(0);
            }
            let action = with_state(|s| {
                // the text was cleared or shifted since that Space: indices are stale
                if s.generation == generation && tray::get(&tray::ENABLED) {
                    s.evaluate(end, false)
                } else {
                    Action::None
                }
            })
            .unwrap_or(Action::None);
            // spelling only gets a word the layout check had nothing to say about
            let layout_is_fine = matches!(action, Action::None);
            perform(window, action);
            if layout_is_fine {
                with_state(|s| {
                    if s.generation == generation {
                        s.request_spelling(end);
                    }
                });
            }
        }
        WM_APP_TYPED => {
            if tray::get(&tray::ENABLED) {
                SetTimer(window, TIMER_IDLE, IDLE_MS, None); // restarts on every key
            }
        }
        WM_TIMER if wparam.0 == TIMER_IDLE => {
            let _ = KillTimer(window, TIMER_IDLE);
            let action = with_state(|s| {
                let finished = s.run.last().map_or(true, |k| k.is_sep());
                if finished || !tray::get(&tray::ENABLED) || !fields::is_safe() {
                    Action::None
                } else {
                    s.evaluate(s.run.len(), true)
                }
            });
            perform(window, action.unwrap_or(Action::None));
        }
        WM_TIMER if wparam.0 == TIMER_HIDE => {
            let rejected = with_state(|s| s.rejected_hint(true)).flatten();
            hide_bubble(window);
            learn_rejection(rejected);
        }
        WM_TIMER if wparam.0 == TIMER_SPELL => hide_spell(window),
        WM_APP_SPELL => {
            let action = with_state(|s| s.on_spell_reply()).unwrap_or(Action::None);
            perform(window, action);
        }
        WM_APP_FIELD => {
            if wparam.0 & fields::FLAG_MOVED != 0 {
                // a word cut off by the move can't be judged or retyped
                with_state(|s| s.skip_word = s.run.last().is_some_and(|k| !k.is_sep()));
                wipe_text(window);
            } else if !fields::is_safe() {
                wipe_text(window);
            }
            if wparam.0 & fields::FLAG_ENTERED != 0 {
                on_field_entered();
            }
        }
        WM_APP_SPELL_ACCEPT => {
            let action = with_state(|s| s.accept_spell(wparam.0 == 1)).unwrap_or(Action::None);
            perform(window, action);
        }
        WM_APP_SPELL_PICK => {
            let action = with_state(|s| {
                s.hover_spell(wparam.0);
                s.accept_spell(false)
            })
            .unwrap_or(Action::None);
            perform(window, action);
        }
        WM_APP_SPELL_HOVER => {
            let action = with_state(|s| s.hover_spell(wparam.0)).unwrap_or(Action::None);
            perform(window, action);
        }
        WM_APP_SPELL_MOVE => {
            let action = with_state(|s| s.move_spell(wparam.0 == 1)).unwrap_or(Action::None);
            perform(window, action);
        }
        WM_APP_HOTKEY | WM_LBUTTONDOWN => {
            let (action, rejected) = with_state(|s| s.on_hotkey()).unwrap_or((Action::None, None));
            perform(window, action);
            learn_rejection(rejected);
        }
        WM_APP_UNDO => {
            let undone = with_state(|s| {
                let fix = s.undone.take()?;
                let switch = s.layout_switch(fix.typed);
                Some((fix.candidate, switch))
            })
            .flatten();
            if let Some((candidate, switch)) = undone {
                if let Some((hkl, target_window)) = switch {
                    let _ = PostMessageW(target_window, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(hkl.0 as isize));
                }
                learn_rejection(candidate);
            }
        }
        WM_APP_RESET => {
            with_state(|s| s.last_fix = None);
            hide_bubble(window);
            hide_spell(window);
        }
        WM_APP_ESCAPE => {
            let rejected = with_state(|s| s.rejected_hint(false)).flatten();
            hide_bubble(window);
            hide_spell(window);
            learn_rejection(rejected);
        }
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
        userwords::load();
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
        EXIT_REQUEST.with(|t| t.set(RegisterWindowMessageW(w!("TypeFlow.Exit"))));
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
                control: 0,
                skip_word: false,
                spell: None,
                spell_pending: None,
                spell_service: None,
                spell_next: 0,
                spell_barrier: 0,
                auto_fix: None,
                undone: None,
            })
        });
        sync_spell();

        let _kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0)?;
        let _mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0)?;
        fields::start(window, WM_APP_FIELD);

        // Debug builds: SMART_KEYBOARD_DEMO="سلام" shows that hint at startup,
        // to look at the glass without typing (simulated keys are ignored).
        #[cfg(debug_assertions)]
        if let Ok(demo) = std::env::var("SMART_KEYBOARD_DEMO") {
            let fa = demo.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));
            show_bubble(window, &demo, if fa { Lang::Fa } else { Lang::En }, 0, None);
            if let Some(Some(b)) = with_state(|s| s.bubble.as_ref().map(|b| b.rect)) {
                println!("demo bubble at {},{} {}x{}", b.left, b.top, b.right - b.left, b.bottom - b.top);
            }
        }

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
            control: 0,
            skip_word: false,
            spell: None,
            spell_pending: None,
            spell_service: None,
            spell_next: 0,
            spell_barrier: 0,
            auto_fix: None,
            undone: None,
        }
    }

    // --------------------------------------------------------- secure fields ---
    // These go through `on_key`, the code the real keyboard hook calls.
    const KEY_A: (u32, u32) = (0x41, 0x1E);
    const KEY_1: (u32, u32) = (0x31, 0x02);
    const KEY_SPACE: (u32, u32) = (0x20, 0x39);

    /// A state already settled on the current window, so the first key
    /// doesn't count as a focus change.
    fn settled() -> State {
        let mut st = state();
        let (hwnd, thread) = foreground();
        st.foreground = hwnd;
        st.control = fields::focused_control(thread);
        st
    }

    fn press(st: &mut State, (vk, scan): (u32, u32)) {
        st.on_key(vk, scan);
    }

    #[test]
    fn the_first_key_in_a_confirmed_field_keeps_its_confirmation() {
        fields::set_safe_for_test(Some(true));
        let mut st = settled();
        st.control = -1; // focus moved since the last key
        let before = fields::pokes_for_test();
        press(&mut st, KEY_A);
        fields::set_safe_for_test(None);
        assert_eq!(fields::pokes_for_test(), before, "must not withdraw a valid confirmation");
        assert_eq!(st.run.len(), 1);
    }

    #[test]
    fn an_unconfirmed_new_field_is_checked_once() {
        fields::set_safe_for_test(Some(false));
        let mut st = settled();
        st.control = -1;
        let before = fields::pokes_for_test();
        press(&mut st, KEY_A);
        press(&mut st, KEY_A);
        fields::set_safe_for_test(None);
        assert_eq!(fields::pokes_for_test(), before + 1, "repeated checks would never finish while typing");
    }

    #[test]
    fn nothing_is_recorded_in_an_unconfirmed_field() {
        fields::set_safe_for_test(Some(false));
        let mut st = settled();
        for key in [KEY_A, KEY_1, KEY_SPACE, KEY_A] {
            press(&mut st, key);
        }
        fields::set_safe_for_test(None);
        assert!(st.run.is_empty());
    }

    #[test]
    fn a_click_on_our_own_popup_keeps_the_text() {
        let mut st = state();
        hint_on(&mut st, "sghl ");
        let rect = RECT { left: 10, top: 10, right: 100, bottom: 40 };
        if let Some(b) = &mut st.bubble {
            b.rect = rect;
        }
        assert!(st.on_own_popup(POINT { x: 50, y: 20 }));
        st.on_click(POINT { x: 50, y: 20 });
        assert_eq!(st.run.len(), 5);
        assert!(!st.on_own_popup(POINT { x: 500, y: 20 }));
        st.on_click(POINT { x: 500, y: 20 });
        assert!(st.run.is_empty());
    }

    #[test]
    fn text_is_dropped_the_moment_the_field_stops_being_safe() {
        fields::set_safe_for_test(Some(true));
        let mut st = settled();
        press(&mut st, KEY_A);
        assert_eq!(st.run.len(), 1);
        st.auto_fix = Some(AutoFix { start: 0, seq: st.seq, generation: st.generation, at: Instant::now(), typed: Lang::En, candidate: None });
        fields::set_safe_for_test(Some(false));
        press(&mut st, KEY_A);
        fields::set_safe_for_test(None);
        assert!(st.run.is_empty() && st.auto_fix.is_none());
    }

    #[test]
    fn a_word_cut_by_the_gate_is_skipped_until_space() {
        fields::set_safe_for_test(Some(false));
        let mut st = settled();
        press(&mut st, KEY_A);
        fields::set_safe_for_test(Some(true));
        press(&mut st, KEY_A);
        assert!(st.run.is_empty(), "the rest of a half-seen word");
        press(&mut st, KEY_SPACE);
        assert!(st.run.is_empty() && !st.skip_word);
        press(&mut st, KEY_A);
        fields::set_safe_for_test(None);
        assert_eq!(st.run.len(), 1);
    }

    #[test]
    fn digits_are_never_recorded() {
        fields::set_safe_for_test(Some(true));
        let mut st = settled();
        press(&mut st, KEY_A);
        press(&mut st, KEY_1);
        assert!(st.run.is_empty(), "the word it was in goes too");
        press(&mut st, KEY_1);
        press(&mut st, KEY_A);
        assert!(st.run.is_empty());
        press(&mut st, KEY_SPACE);
        press(&mut st, KEY_1);
        fields::set_safe_for_test(None);
        assert!(st.run.is_empty());
    }

    #[test]
    fn right_ctrl_is_not_tracked_in_a_secure_field() {
        fields::set_safe_for_test(Some(false));
        let mut st = settled();
        st.on_key(VK_RCTRL_CODE, 0x1D);
        fields::set_safe_for_test(None);
        assert!(!st.rctrl_down && !st.rctrl_alone);
    }

    /// Simulates typing `s` with the English layout active.
    fn type_en(st: &mut State, s: &str) {
        for ch in s.chars() {
            let c = ch.to_string();
            st.run.push(Key::new(c.clone(), mapping::en_to_fa(&c), Lang::En));
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

    #[test]
    fn half_space_is_part_of_a_word() {
        assert!(!Key::new("_".into(), "\u{200c}".into(), Lang::Fa).is_sep());
        assert!(Key::new(".".into(), "/".into(), Lang::Fa).is_sep());
    }

    #[test]
    fn punctuation_around_a_word_is_a_separator() {
        let mut st = state();
        type_en(&mut st, "(sghl)");
        let end = st.run.len();
        assert_eq!(shown(st.evaluate(end, false)).as_deref(), Some("(سلام)"));
        let mut st = state();
        type_en(&mut st, "sghl.");
        let end = st.run.len();
        assert_eq!(shown(st.evaluate(end, false)).as_deref(), Some("سلام."));
    }

    // ------------------------------------------------------------- spelling ---
    fn spell_word(typed: &str) -> Option<String> {
        let mut st = state();
        type_en(&mut st, typed);
        let end = st.run.len();
        st.spell_target(end).map(|(t, _)| t.word)
    }

    #[test]
    fn spelling_takes_the_plain_word_before_the_space() {
        assert_eq!(spell_word("recieve ").as_deref(), Some("recieve"));
        assert_eq!(spell_word("(recieve), ").as_deref(), Some("recieve"));
        assert_eq!(spell_word("i recieve ").as_deref(), Some("recieve"));
        assert_eq!(spell_word("Recieve ").as_deref(), Some("Recieve"));
        assert_eq!(spell_word("done. Recieve ").as_deref(), Some("Recieve"));
    }

    #[test]
    fn spelling_skips_what_is_not_a_plain_word() {
        for typed in ["recieve", "a@example.com ", "example.com/page ", "abc123 ", "NASA ", "camelCase ", "my Tehran ", "ab "] {
            assert_eq!(spell_word(typed), None, "{typed}");
        }
    }

    #[test]
    fn spelling_reads_persian_keys() {
        let mut st = state();
        for c in "خاهش ".chars() {
            let c = c.to_string();
            st.run.push(Key::new(mapping::fa_to_en(&c), c, Lang::Fa));
        }
        let end = st.run.len();
        let (target, other) = st.spell_target(end).unwrap();
        assert_eq!((target.word.as_str(), other.as_str(), target.lang), ("خاهش", "ohia", Lang::Fa));
    }

    fn choose(st: &mut State, end: usize, word: &str) {
        let (target, _) = st.spell_target(end).unwrap();
        let items = vec![Suggestion { word: word.into(), confidence: 0.9 }];
        st.spell = Some(SpellChoice { target, items, selected: 0, rect: RECT::default(), navigated: false });
    }

    #[test]
    fn accepting_replaces_the_word_and_keeps_later_typing() {
        let mut st = state();
        type_en(&mut st, "recieve ");
        let end = st.run.len();
        type_en(&mut st, "it");
        choose(&mut st, end, "receive");
        match st.accept_spell(false) {
            Action::Replace { backspaces, text } => assert_eq!((backspaces, text.as_str()), (10, "receive it")),
            _ => panic!("expected a replacement"),
        }
        assert_eq!(st.text_of(0, st.run.len()), "receive it");
        assert!(st.spell.is_none());
    }

    #[test]
    fn a_word_of_another_length_keeps_the_text_in_sync() {
        let mut st = state();
        type_en(&mut st, "helllo ");
        let end = st.run.len();
        choose(&mut st, end, "hello");
        assert!(matches!(st.accept_spell(false), Action::Replace { backspaces: 7, .. }));
        assert_eq!(st.run.len(), 6);
    }

    fn two_choices(st: &mut State, end: usize) {
        let (target, _) = st.spell_target(end).unwrap();
        let items = vec![
            Suggestion { word: "receive".into(), confidence: 0.9 },
            Suggestion { word: "relieve".into(), confidence: 0.1 },
        ];
        st.spell = Some(SpellChoice { target, items, selected: 0, rect: RECT::default(), navigated: false });
    }

    #[test]
    fn arrows_then_space_applies_the_choice_with_one_space() {
        let mut st = state();
        type_en(&mut st, "recieve ");
        let end = st.run.len();
        two_choices(&mut st, end);
        st.move_spell(false);
        assert!(st.spell.as_ref().unwrap().navigated);
        match st.accept_spell(true) {
            Action::Replace { backspaces, text } => assert_eq!((backspaces, text.as_str()), (8, "relieve ")),
            _ => panic!("expected a replacement"),
        }
        assert_eq!(st.text_of(0, st.run.len()), "relieve ");
    }

    #[test]
    fn space_adds_one_when_more_was_typed_after_the_word() {
        let mut st = state();
        type_en(&mut st, "recieve ");
        let end = st.run.len();
        two_choices(&mut st, end);
        type_en(&mut st, "it");
        match st.accept_spell(true) {
            Action::Replace { text, .. } => assert_eq!(text, "receive it "),
            _ => panic!("expected a replacement"),
        }
        assert_eq!(st.text_of(0, st.run.len()), "receive it ");
    }

    #[test]
    fn hovering_does_not_arm_space() {
        let mut st = state();
        type_en(&mut st, "recieve ");
        let end = st.run.len();
        two_choices(&mut st, end);
        st.hover_spell(1);
        assert!(!st.spell.as_ref().unwrap().navigated);
    }

    #[test]
    fn an_edited_word_is_not_replaced() {
        let mut st = state();
        type_en(&mut st, "recieve ");
        let end = st.run.len();
        choose(&mut st, end, "receive");
        st.run.truncate(3);
        assert!(matches!(st.accept_spell(false), Action::HideSpell));
        assert_eq!(st.run.len(), 3);
    }

    #[test]
    fn text_with_a_possible_half_space_is_not_retyped() {
        let mut st = state();
        type_en(&mut st, "recieve ");
        let end = st.run.len();
        choose(&mut st, end, "receive");
        type_en(&mut st, "x ");
        st.spell_barrier = st.run.len();
        assert!(matches!(st.accept_spell(false), Action::HideSpell));
    }

    // ------------------------------------------------------------- learning ---
    fn hint_on(st: &mut State, typed: &str) {
        type_en(st, typed);
        let end = st.run.len();
        let Action::Show { start, target, candidate, .. } = st.evaluate(end, false) else { panic!("no hint") };
        st.bubble = Some(Bubble { start, target, rect: RECT::default(), caret: RECT::default(), candidate });
    }

    #[test]
    fn esc_rejects_a_hint_whose_word_is_untouched() {
        let mut st = state();
        hint_on(&mut st, "sghl ");
        let c = st.rejected_hint(false).unwrap();
        assert_eq!((c.word.as_str(), c.lang), ("sghl", Lang::En));
    }

    #[test]
    fn a_timeout_counts_only_after_typing_on() {
        let mut st = state();
        hint_on(&mut st, "sghl ");
        assert!(st.rejected_hint(true).is_none(), "may never have been seen");
        type_en(&mut st, "there");
        assert_eq!(st.rejected_hint(true).unwrap().word, "sghl");
    }

    #[test]
    fn fixing_the_word_by_hand_is_not_a_rejection() {
        let mut st = state();
        hint_on(&mut st, "sghl ");
        st.run.truncate(2);
        type_en(&mut st, "xy ");
        assert!(st.rejected_hint(false).is_none());
    }

    #[test]
    fn a_hint_for_a_phrase_teaches_nothing() {
        let mut st = state();
        type_en(&mut st, "di v, n, fh ");
        let end = st.run.len();
        let Action::Show { candidate, .. } = st.evaluate(end, false) else { panic!("no hint") };
        assert!(candidate.is_none());
    }

    #[test]
    fn a_rejected_word_is_left_out_of_the_next_hint() {
        let mut st = state();
        hint_on(&mut st, "sghl ");
        st.rejected_hint(false);
        st.bubble = None;
        type_en(&mut st, "o,fd ");
        let end = st.run.len();
        assert_eq!(shown(st.evaluate(end, false)).as_deref(), Some("خوبی"));
    }

    fn auto_fixed(st: &mut State) {
        type_en(st, "sghl ");
        let candidate = Candidate { start: 0, end: 4, generation: st.generation, lang: Lang::En, word: "sghl".into() };
        let _ = st.convert_from(0, Lang::Fa);
        st.auto_fix = Some(AutoFix { start: 0, seq: st.seq, generation: st.generation, at: Instant::now(), typed: Lang::En, candidate: Some(candidate) });
    }

    #[test]
    fn the_hotkey_undoes_an_auto_fix_and_rejects_the_word() {
        let mut st = state();
        auto_fixed(&mut st);
        let (action, rejected) = st.on_hotkey();
        match action {
            Action::Convert { backspaces, text, .. } => assert_eq!((backspaces, text.as_str()), (5, "sghl ")),
            _ => panic!("expected the original text back"),
        }
        assert_eq!(rejected.unwrap().word, "sghl");
        assert!(st.run.iter().all(|k| k.kept && k.lang == Lang::En));
    }

    #[test]
    fn the_hotkey_is_no_undo_once_typing_went_on() {
        let mut st = state();
        auto_fixed(&mut st);
        st.seq += 1;
        let (_, rejected) = st.on_hotkey();
        assert!(rejected.is_none());
    }

    #[test]
    fn an_old_auto_fix_cannot_be_undone() {
        let mut st = state();
        auto_fixed(&mut st);
        if let Some(fix) = &mut st.auto_fix {
            fix.at = Instant::now() - UNDO_WINDOW - Duration::from_secs(1);
        }
        assert!(st.fresh_auto_fix().is_none());
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
    fn all_sounds_are_valid_wavs() {
        for wav in [LAYOUT_WAV, TYPO_WAV, FIELD_WAV] {
            assert_eq!(&wav[..4], b"RIFF");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize, wav.len() - 8);
        }
        assert_ne!(LAYOUT_WAV, TYPO_WAV);
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

    /// The bubble as DWM would show it over a flat backdrop of `under`: the
    /// acrylic tint (theme.rs) first, then the pixels from `render_bubble`.
    unsafe fn preview_bubble(text: &str, rtl: bool, theme: &theme::Theme, under: (u8, u8, u8)) -> (i32, i32, Vec<u32>) {
        let c = theme.colors();
        let (w, h, px) = super::render_bubble(text, rtl, &c);
        let ta = c.fill_alpha as f64 / 255.0;
        let glass = |f: u8, u: u8| f as f64 * ta + u as f64 * (1.0 - ta);
        let back = (glass(c.fill.0, under.0), glass(c.fill.1, under.1), glass(c.fill.2, under.2));
        let out = px
            .iter()
            .map(|p| {
                let a = (p >> 24) as f64 / 255.0;
                let ch = |s: u32, b: f64| (((p >> s) & 0xFF) as f64 + b * (1.0 - a)).round().min(255.0) as u32;
                ch(16, back.0) << 16 | ch(8, back.1) << 8 | ch(0, back.2)
            })
            .collect();
        (w, h, out)
    }

    #[test]
    #[ignore]
    fn previews() {
        load_fonts();
        let light = theme::Theme { accent_on_surfaces: true, ..theme::Theme::DEFAULT };
        let dark = theme::Theme { light: false, ..light };
        unsafe {
            let (w, h, px) = preview_bubble("سلام خوبی؟", true, &light, (0xDB, 0xE4, 0xEF));
            save_bmp("target/preview/bubble_fa.bmp", w, h, &px);
            let (w, h, px) = preview_bubble("hello", false, &dark, (0x10, 0x17, 0x22));
            save_bmp("target/preview/bubble_en.bmp", w, h, &px);
        }
    }
}
