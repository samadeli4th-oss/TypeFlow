//! Watches which control has focus and decides whether keys may be read there.
//!
//! The rule (see CLAUDE.md): keys are only looked at in a control positively
//! confirmed to be an ordinary text box. Anything else counts as secure:
//! password, number and terminal input, and every control that can't be
//! identified. Whatever might have moved focus withdraws the confirmation
//! at once (`poke`), and it only comes back after the new control is checked.
//!
//! This runs on its own thread because accessibility calls go to the other
//! app and wait for it: on the main thread, a hung app would hold up the
//! keyboard hook and with it all typing. Only a control's role, state and input
//! type are read, never its name or contents.
//!
//! Chrome, Edge and Electron apps send no focus events until an accessibility
//! client has asked for their page, so each time a window comes forward or is
//! clicked, its page is asked once and its focused element is checked.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering};

use windows::core::{Interface, BSTR, GUID, HRESULT, VARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, IDispatch, IServiceProvider, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromEvent, AccessibleObjectFromWindow, IAccessible, SetWinEventHook, WindowFromAccessibleObject,
    HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::FieldAction;
use crate::tray;

/// Bit of the wparam posted to the main window: focus entered a password or
/// number field.
pub const FLAG_ENTERED: usize = 4;
/// Focus is on a different element than at the last report.
pub const FLAG_MOVED: usize = 8;

const WM_CHECK: u32 = WM_APP + 1;
/// Gives the app a moment to move focus after a click or window switch.
const CHECK_DELAY_MS: u32 = 120;
/// A page that's still loading may not know its focus yet: ask again.
const MAX_TRIES: u32 = 8;
/// Browsers and Electron apps hold many fields in one window and don't reliably
/// announce focus moving between them (Firefox not at all), so their focused
/// element is looked at on a timer while they're in front.
const POLLED_CLASSES: [&str; 2] = ["MozillaWindowClass", "Chrome_WidgetWin_1"];
const POLL_MS: u32 = 100;

const STATE_PROTECTED: i32 = 0x2000_0000;
const ROLE_SPIN_BUTTON: i32 = 0x34;
const ROLE_TEXT: i32 = 0x2A;
/// Document editors that report a generic role but are never credential fields.
const EDITOR_CLASSES: [&str; 3] = ["_WwG", "Scintilla", "RichEditD2DPT"];
/// Their password prompts look like ordinary input.
const TERMINAL_CLASSES: [&str; 7] = [
    "ConsoleWindowClass",
    "CASCADIA_HOSTING_WINDOW_CLASS",
    "mintty",
    "PuTTY",
    "VirtualConsoleClass",
    "Alacritty",
    "org.wezfurlong.wezterm",
];
/// The input element of terminals inside Electron apps such as VS Code.
const WEB_TERMINAL_CLASS: &str = "xterm-helper-textarea";
const IID_IACCESSIBLE2: GUID = GUID::from_u128(0xE89F726E_C4F4_4C19_BB19_B647D7FA8478);
// IAccessible2 vtable slots (IUnknown 3 + IDispatch 4 + IAccessible 21 = 28 first)
const IA2_GET_STATES: usize = 35;
const IA2_GET_UNIQUE_ID: usize = 41;
const IA2_GET_ATTRIBUTES: usize = 45;
const IA2_STATE_EDITABLE: i32 = 0x8;

static THREAD: AtomicU32 = AtomicU32::new(0);
/// Goes up whenever focus may have moved.
static EPOCH: AtomicU64 = AtomicU64::new(1);
/// The epoch whose focused control was confirmed safe (0 = none).
static SAFE_EPOCH: AtomicU64 = AtomicU64::new(0);
/// The control that confirmation is for.
static SAFE_CONTROL: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static TARGET: Cell<(isize, u32)> = const { Cell::new((0, 0)) };
    /// The field focus is in, so one field only counts as entered once.
    static LAST: Cell<Option<(isize, i64)>> = const { Cell::new(None) };
    /// The element the last report was about.
    static LAST_ELEMENT: Cell<Option<(isize, i64)>> = const { Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    static SAFE_FOR_TEST: Cell<Option<bool>> = const { Cell::new(None) };
    static POKES_FOR_TEST: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
pub fn pokes_for_test() -> u32 {
    POKES_FOR_TEST.with(Cell::get)
}

#[cfg(test)]
pub fn set_safe_for_test(safe: Option<bool>) {
    SAFE_FOR_TEST.with(|s| s.set(safe));
}

/// True only while the focused control is confirmed to be an ordinary text box.
/// Safe to call from a hook: it makes no calls to other apps.
pub fn is_safe() -> bool {
    #[cfg(test)]
    if let Some(safe) = SAFE_FOR_TEST.with(Cell::get) {
        return safe;
    }
    SAFE_EPOCH.load(Ordering::SeqCst) == EPOCH.load(Ordering::SeqCst)
}

/// `is_safe`, and the confirmation is for this very control.
pub fn is_safe_for(control: isize) -> bool {
    #[cfg(test)]
    if let Some(safe) = SAFE_FOR_TEST.with(Cell::get) {
        return safe;
    }
    is_safe() && SAFE_CONTROL.load(Ordering::SeqCst) == control
}

/// The control with keyboard focus in `thread`. Doesn't wait on the other app.
pub fn focused_control(thread: u32) -> isize {
    unsafe {
        let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(thread, &mut info).is_ok() {
            info.hwndFocus.0 as isize
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Field {
    password: bool,
    number: bool,
    /// Keys may be read here.
    safe: bool,
}

/// What could be learned about a control. Never its name or contents.
#[derive(Default)]
struct Facts<'a> {
    /// `None` when the control wouldn't say.
    state: Option<i32>,
    role: Option<i32>,
    input_type: Option<&'a str>,
    number_edit: bool,
    password_edit: bool,
    editor: bool,
    /// The browser marks it as editable (rich text editors on web pages).
    editable: bool,
    terminal: bool,
}

fn classify(f: &Facts) -> Field {
    let protected = f.state.is_some_and(|s| s & STATE_PROTECTED != 0);
    let password = protected || f.password_edit || f.input_type == Some("password");
    let number = !password
        && (f.role == Some(ROLE_SPIN_BUTTON) || f.number_edit || matches!(f.input_type, Some("number" | "tel")));
    let plain_type = matches!(f.input_type, None | Some("text" | "search" | "url" | "email"));
    let text_box = f.role == Some(ROLE_TEXT) || (f.role.is_some() && (f.editor || f.editable));
    Field {
        password,
        number,
        safe: f.state.is_some() && text_box && plain_type && !password && !number && !f.terminal,
    }
}

/// Whether IAccessible2's `class:a b;` attribute includes `name`.
fn has_class(attributes: &str, name: &str) -> bool {
    attributes
        .split(';')
        .find_map(|part| part.strip_prefix("class:"))
        .is_some_and(|classes| classes.split_whitespace().any(|c| c == name))
}

/// `text-input-type:number` out of IAccessible2's `key:value;` attributes.
fn input_type(attributes: &str) -> Option<&str> {
    attributes.split(';').find_map(|part| part.strip_prefix("text-input-type:"))
}

/// Whether moving focus to `key` counts as entering a field.
fn entered(last: Option<(isize, i64)>, key: (isize, i64), field: Field) -> bool {
    (field.password || field.number) && last != Some(key)
}

fn wanted() -> bool {
    tray::active() || tray::field_action() != FieldAction::Disable
}

/// Starts the watcher. `message` is posted to `window` after each check, with
/// FLAG_* in wparam.
pub fn start(window: HWND, message: u32) {
    let target = window.0 as isize;
    let _ = std::thread::Builder::new().name("typeflow-fields".into()).spawn(move || run(target, message));
}

/// Focus may have moved: nothing is safe until the control has been checked
/// again. Safe to call from a hook.
pub fn poke() {
    #[cfg(test)]
    POKES_FOR_TEST.with(|p| p.set(p.get() + 1));
    EPOCH.fetch_add(1, Ordering::SeqCst);
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, WM_CHECK, WPARAM(0), LPARAM(0));
        }
    }
}

fn run(window: isize, message: u32) {
    TARGET.with(|t| t.set((window, message)));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut msg = MSG::default();
        // creates this thread's message queue before anyone posts to it
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
        let _focus = SetWinEventHook(EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS, HMODULE::default(), Some(on_focus), 0, 0, WINEVENT_OUTOFCONTEXT);
        let _foreground = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, HMODULE::default(), Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT);
        THREAD.store(GetCurrentThreadId(), Ordering::Release);
        poke();

        let (mut timer, mut tries) = (0usize, 0u32);
        let poll = SetTimer(None, 0, POLL_MS, None);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.is_invalid() && msg.message == WM_CHECK {
                tries = 0;
                timer = SetTimer(None, timer, CHECK_DELAY_MS, None);
            } else if msg.hwnd.is_invalid() && msg.message == WM_TIMER && msg.wParam.0 == poll {
                if wanted() && POLLED_CLASSES.contains(&class_name(GetForegroundWindow()).as_str()) {
                    check_focus();
                }
            } else if msg.hwnd.is_invalid() && msg.message == WM_TIMER && timer != 0 && msg.wParam.0 == timer {
                tries += 1;
                // after the last try the field simply stays unconfirmed
                if check_focus() || tries >= MAX_TRIES {
                    let _ = KillTimer(None, timer);
                    timer = 0;
                }
            } else {
                DispatchMessageW(&msg);
            }
        }
    }
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    poke();
}

unsafe extern "system" fn on_focus(_: HWINEVENTHOOK, _: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if GetAncestor(hwnd, GA_ROOT) != GetForegroundWindow() {
        return;
    }
    if object != OBJID_CLIENT.0 || !wanted() {
        poke();
        return;
    }
    let epoch = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    let mut acc = None;
    let mut id = VARIANT::default();
    if AccessibleObjectFromEvent(hwnd, object as u32, child as u32, &mut acc, &mut id).is_err() {
        poke();
        return;
    }
    match acc {
        Some(acc) => report(hwnd, child, &acc, &id, epoch),
        None => poke(),
    }
}

/// What has focus inside `window`, waking its accessibility support if needed.
unsafe fn focused_in(window: HWND) -> Option<(IAccessible, VARIANT, i32)> {
    let acc = client_object(window)?;
    let _ = accessible2(&acc); // in Chromium, this request turns accessibility on
    let focus = acc.accFocus().ok()?;
    if let Ok(element) = IDispatch::try_from(&focus).and_then(|d| d.cast::<IAccessible>()) {
        return Some((element, VARIANT::from(0i32), 0));
    }
    let child = i32::try_from(&focus).ok()?;
    Some((acc, focus, child))
}

/// Checks the focused control. Returns false if a browser couldn't say what
/// has focus yet. Until it can, the field stays unconfirmed.
unsafe fn check_focus() -> bool {
    // read first: if focus moves during the check, its answer no longer counts
    let epoch = EPOCH.load(Ordering::SeqCst);
    let foreground = GetForegroundWindow();
    if !wanted() || foreground.is_invalid() {
        not_safe();
        return true;
    }
    let mut pages: Vec<HWND> = Vec::new();
    let _ = EnumChildWindows(foreground, Some(collect_page), LPARAM(&mut pages as *mut Vec<HWND> as isize));
    if !pages.is_empty() {
        // the page first; the browser's own controls (address bar) after it
        for window in pages.iter().copied().chain([foreground]) {
            if let Some((element, id, child)) = focused_in(window) {
                report(window, child, &element, &id, epoch);
                return true;
            }
        }
        return false;
    }
    let thread = GetWindowThreadProcessId(foreground, None);
    let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    let focused = if GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndFocus.is_invalid() {
        info.hwndFocus
    } else {
        foreground
    };
    // the window with focus may only host the real control (Notepad, Firefox)
    if let Some((element, id, child)) = focused_in(focused) {
        report(owner_of(&element).unwrap_or(focused), child, &element, &id, epoch);
        return true;
    }
    match client_object(focused) {
        Some(acc) => report(focused, 0, &acc, &VARIANT::from(0i32), epoch),
        None => not_safe(),
    }
    true
}

/// The window that really holds an accessible element.
unsafe fn owner_of(element: &IAccessible) -> Option<HWND> {
    let mut window = HWND::default();
    WindowFromAccessibleObject(element, Some(&mut window)).ok()?;
    (!window.is_invalid()).then_some(window)
}

unsafe fn not_safe() {
    SAFE_EPOCH.store(0, Ordering::SeqCst);
    send(0);
}

unsafe fn send(flags: usize) {
    let (target, message) = TARGET.with(Cell::get);
    let _ = PostMessageW(HWND(target as *mut c_void), message, WPARAM(flags), LPARAM(0));
}

unsafe extern "system" fn collect_page(window: HWND, list: LPARAM) -> BOOL {
    let mut class = [0u16; 64];
    let n = GetClassNameW(window, &mut class) as usize;
    if String::from_utf16_lossy(&class[..n]).starts_with("Chrome_RenderWidgetHostHWND") {
        (*(list.0 as *mut Vec<HWND>)).push(window);
    }
    TRUE
}

unsafe fn client_object(window: HWND) -> Option<IAccessible> {
    let mut acc: Option<IAccessible> = None;
    AccessibleObjectFromWindow(window, OBJID_CLIENT.0 as u32, &IAccessible::IID, &mut acc as *mut _ as *mut *mut c_void).ok()?;
    acc
}

/// (attributes, unique id, editable) from IAccessible2, which browsers implement.
unsafe fn accessible2(acc: &IAccessible) -> Option<(String, i32, bool)> {
    let service = acc.cast::<IServiceProvider>().ok()?;
    let mut object: *mut c_void = std::ptr::null_mut();
    (Interface::vtable(&service).QueryService)(Interface::as_raw(&service), &IAccessible::IID, &IID_IACCESSIBLE2, &mut object)
        .ok()
        .ok()?;
    if object.is_null() {
        return None;
    }
    let table = *(object as *const *const *const c_void);
    let get_attributes: unsafe extern "system" fn(*mut c_void, *mut BSTR) -> HRESULT =
        std::mem::transmute(*table.add(IA2_GET_ATTRIBUTES));
    let get_unique_id: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT =
        std::mem::transmute(*table.add(IA2_GET_UNIQUE_ID));
    let get_states: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT =
        std::mem::transmute(*table.add(IA2_GET_STATES));
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*table.add(2));
    let mut attributes = BSTR::new();
    let mut id = 0;
    let mut states = 0;
    let have_attributes = get_attributes(object, &mut attributes).is_ok();
    let have_id = get_unique_id(object, &mut id).is_ok();
    let editable = get_states(object, &mut states).is_ok() && states & IA2_STATE_EDITABLE != 0;
    release(object);
    (have_attributes && have_id).then(|| (attributes.to_string(), id, editable))
}

unsafe fn class_name(window: HWND) -> String {
    let mut class = [0u16; 64];
    let n = GetClassNameW(window, &mut class).max(0) as usize;
    String::from_utf16_lossy(&class[..n])
}

/// What kind of field this is, and its IAccessible2 id if it has one.
unsafe fn inspect(window: HWND, acc: &IAccessible, id: &VARIANT) -> (Field, Option<i32>) {
    let state = acc.get_accState(id).ok().and_then(|v| i32::try_from(&v).ok());
    let role = acc.get_accRole(id).ok().and_then(|v| i32::try_from(&v).ok());
    let extra = accessible2(acc);
    let attributes = extra.as_ref().map_or("", |(attributes, _, _)| attributes.as_str());
    let class = class_name(window);
    let root = class_name(GetAncestor(window, GA_ROOT));
    let is_edit = class.to_ascii_lowercase().contains("edit");
    let style = GetWindowLongW(window, GWL_STYLE);
    let field = classify(&Facts {
        state,
        role,
        input_type: input_type(attributes),
        number_edit: is_edit && style & ES_NUMBER != 0,
        password_edit: is_edit && style & ES_PASSWORD != 0,
        editor: EDITOR_CLASSES.contains(&class.as_str()),
        editable: extra.as_ref().is_some_and(|(_, _, editable)| *editable),
        terminal: TERMINAL_CLASSES.contains(&class.as_str())
            || TERMINAL_CLASSES.contains(&root.as_str())
            || has_class(attributes, WEB_TERMINAL_CLASS),
    });
    (field, extra.as_ref().map(|(_, unique, _)| *unique))
}

unsafe fn report(window: HWND, child: i32, acc: &IAccessible, id: &VARIANT, epoch: u64) {
    // read first: if focus moves during the check, the confirmation won't match it
    let control = focused_control(GetWindowThreadProcessId(GetForegroundWindow(), None));
    let (field, unique) = inspect(window, acc, id);
    // browsers: one window, many fields, each with its own id
    let key = match unique {
        Some(unique) => (GetAncestor(window, GA_ROOT).0 as isize, unique as i64),
        None => (window.0 as isize, child as i64),
    };
    let is_new = entered(LAST.with(Cell::get), key, field);
    LAST.with(|l| l.set((field.password || field.number).then_some(key)));
    let moved = LAST_ELEMENT.with(|l| l.replace(Some(key))).is_some_and(|last| last != key);
    SAFE_CONTROL.store(control, Ordering::SeqCst);
    SAFE_EPOCH.store(if field.safe { epoch } else { 0 }, Ordering::SeqCst);
    send(if is_new { FLAG_ENTERED } else { 0 } | if moved { FLAG_MOVED } else { 0 });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_box<'a>() -> Facts<'a> {
        Facts { state: Some(0), role: Some(ROLE_TEXT), ..Facts::default() }
    }

    #[test]
    fn recognizes_password_fields() {
        for facts in [
            Facts { state: Some(STATE_PROTECTED), ..text_box() },
            Facts { input_type: Some("password"), ..text_box() },
            Facts { password_edit: true, ..text_box() },
        ] {
            let field = classify(&facts);
            assert!(field.password && !field.safe);
        }
    }

    #[test]
    fn recognizes_number_fields() {
        for facts in [
            Facts { role: Some(ROLE_SPIN_BUTTON), input_type: Some("number"), ..text_box() },
            Facts { input_type: Some("tel"), ..text_box() },
            Facts { number_edit: true, ..text_box() },
        ] {
            let field = classify(&facts);
            assert!(field.number && !field.safe);
        }
    }

    #[test]
    fn plain_text_fields_are_safe() {
        for kind in [None, Some("text"), Some("email"), Some("search"), Some("url")] {
            let field = classify(&Facts { input_type: kind, ..text_box() });
            assert_eq!(field, Field { password: false, number: false, safe: true }, "{kind:?}");
        }
    }

    #[test]
    fn anything_unconfirmed_is_secure() {
        const CLIENT: i32 = 0x0A;
        for facts in [
            Facts::default(),
            Facts { state: None, ..text_box() },
            Facts { role: None, ..text_box() },
            Facts { role: Some(CLIENT), ..text_box() },
            Facts { input_type: Some("some-future-type"), ..text_box() },
            Facts { state: None, editor: true, ..text_box() },
        ] {
            assert!(!classify(&facts).safe);
        }
    }

    #[test]
    fn a_field_is_secure_until_checked_and_after_focus_may_move() {
        assert!(!is_safe(), "nothing has been confirmed");
        let confirmed = EPOCH.load(Ordering::SeqCst);
        poke();
        assert_ne!(EPOCH.load(Ordering::SeqCst), confirmed, "an older confirmation no longer matches");
    }

    #[test]
    fn terminals_are_secure_even_as_text_boxes() {
        assert!(!classify(&Facts { terminal: true, ..text_box() }).safe);
        assert!(has_class("tag:textarea;class:foo xterm-helper-textarea;", WEB_TERMINAL_CLASS));
        assert!(!has_class("tag:textarea;class:inputarea;", WEB_TERMINAL_CLASS));
    }

    #[test]
    fn web_rich_text_editors_count_as_text_but_not_when_secure() {
        const GROUPING: i32 = 0x14;
        const BUTTON: i32 = 0x2B;
        let editor = |facts: Facts| classify(&Facts { role: Some(GROUPING), editable: true, ..facts }).safe;
        assert!(editor(text_box()));
        assert!(!editor(Facts { state: Some(STATE_PROTECTED), ..text_box() }));
        assert!(!editor(Facts { terminal: true, ..text_box() }));
        assert!(!editor(Facts { state: None, ..text_box() }));
        assert!(!classify(&Facts { role: Some(BUTTON), ..text_box() }).safe, "not editable");
    }

    #[test]
    fn known_document_editors_count_as_text() {
        const CLIENT: i32 = 0x0A;
        assert!(classify(&Facts { role: Some(CLIENT), editor: true, ..text_box() }).safe);
    }

    #[test]
    fn real_windows_controls_are_classified() {
        use windows::core::w;
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let _ = LoadLibraryW(w!("msftedit.dll"));
            let instance = GetModuleHandleW(None).unwrap();
            let field_of = |class: windows::core::PCWSTR, style: i32| {
                let window = CreateWindowExW(WINDOW_EX_STYLE(0), class, None, WS_POPUP | WINDOW_STYLE(style as u32),
                    0, 0, 100, 30, None, None, instance, None).unwrap();
                let acc = client_object(window).unwrap();
                let (field, _) = inspect(window, &acc, &VARIANT::from(0i32));
                let _ = DestroyWindow(window);
                field
            };
            assert!(field_of(w!("EDIT"), 0).safe, "plain edit box");
            assert!(field_of(w!("RICHEDIT50W"), 0).safe, "rich edit box");
            let password = field_of(w!("EDIT"), ES_PASSWORD);
            assert!(password.password && !password.safe, "password box");
            let rich_password = field_of(w!("RICHEDIT50W"), ES_PASSWORD);
            assert!(rich_password.password && !rich_password.safe, "rich password box");
            let number = field_of(w!("EDIT"), ES_NUMBER);
            assert!(number.number && !number.safe, "number box");
            assert!(!field_of(w!("BUTTON"), 0).safe, "button");
            assert!(!field_of(w!("STATIC"), 0).safe, "label");
        }
    }

    #[test]
    fn reads_the_input_type() {
        assert_eq!(input_type("tag:input;text-input-type:tel;class:a"), Some("tel"));
        assert_eq!(input_type("tag:textarea"), None);
    }

    #[test]
    fn a_field_counts_as_entered_once() {
        let password = Field { password: true, number: false, safe: false };
        assert!(entered(None, (1, 5), password));
        assert!(!entered(Some((1, 5)), (1, 5), password));
        assert!(entered(Some((1, 5)), (1, 6), password));
        assert!(!entered(None, (1, 7), Field::default()));
    }
}
