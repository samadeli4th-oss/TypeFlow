//! Watches which control has focus and reports password fields.
//!
//! This runs on its own thread because accessibility calls go to the other
//! app and wait for it: on the main thread, a hung app would hold up the
//! keyboard hook and with it all typing. Only a control's state is read, never
//! its name or contents.
//!
//! Chrome, Edge and Electron apps send no focus events until an accessibility
//! client has asked for their page, so each time a window comes forward, is
//! clicked, or gets new text, its page is asked and its focused element checked.

use std::ffi::c_void;
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use windows::core::{IUnknown, Interface, BSTR, GUID, HRESULT, VARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, IDispatch, IServiceProvider, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{
    AccessibleObjectFromEvent, AccessibleObjectFromWindow, IAccessible, SetWinEventHook, HWINEVENTHOOK,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::tray;

/// Bits of the wparam posted to the main window.
pub const FLAG_PASSWORD: usize = 1;
/// Nothing new is known about the field: keep treating it as before.
pub const FLAG_UNCHANGED: usize = 2;

const WM_CHECK: u32 = WM_APP + 1;
/// Gives the app a moment to move focus after a click or window switch.
const CHECK_DELAY_MS: u32 = 120;
/// A page that's still loading may not know its focus yet: ask again.
const MAX_TRIES: u32 = 8;

const STATE_PROTECTED: i32 = 0x2000_0000;
const IID_IACCESSIBLE2: GUID = GUID::from_u128(0xE89F726E_C4F4_4C19_BB19_B647D7FA8478);
/// IAccessible2 vtable slot (IUnknown 3 + IDispatch 4 + IAccessible 21 = 28 first).
const IA2_GET_ATTRIBUTES: usize = 45;

static THREAD: AtomicU32 = AtomicU32::new(0);
/// Counts check requests, so a reply says which requests it answers.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static TARGET: Cell<(isize, u32)> = const { Cell::new((0, 0)) };
}

/// `attributes` is IAccessible2's `key:value;` list, if the app has one.
fn flags_for(state: i32, attributes: Option<&str>) -> usize {
    let typed_password = attributes.is_some_and(|a| a.split(';').any(|part| part == "text-input-type:password"));
    if state & STATE_PROTECTED != 0 || typed_password { FLAG_PASSWORD } else { 0 }
}

fn wanted() -> bool {
    tray::get(&tray::ENABLED)
}

/// Starts the watcher. `message` is posted to `window` with FLAG_* in wparam
/// and, in lparam, the newest `poke` number the report answers (0 = none).
pub fn start(window: HWND, message: u32) {
    let target = window.0 as isize;
    let _ = std::thread::Builder::new().name("typeflow-fields".into()).spawn(move || run(target, message));
}

/// Asks for a check of the focused control and returns the request's number
/// (0 if the watcher isn't running). Safe to call from a hook.
pub fn poke() -> u64 {
    let thread = THREAD.load(Ordering::Acquire);
    if thread == 0 {
        return 0;
    }
    let request = REQUESTS.fetch_add(1, Ordering::SeqCst) + 1;
    unsafe {
        let _ = PostThreadMessageW(thread, WM_CHECK, WPARAM(0), LPARAM(0));
    }
    request
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
        let _ = poke();

        let (mut timer, mut tries) = (0usize, 0u32);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.is_invalid() && msg.message == WM_CHECK {
                tries = 0;
                timer = SetTimer(None, timer, CHECK_DELAY_MS, None);
            } else if msg.hwnd.is_invalid() && msg.message == WM_TIMER && timer != 0 && msg.wParam.0 == timer {
                tries += 1;
                // read first: requests arriving during the check aren't answered by it
                let request = REQUESTS.load(Ordering::SeqCst);
                if check_focus(request) || tries >= MAX_TRIES {
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
    let _ = poke();
}

unsafe extern "system" fn on_focus(_: HWINEVENTHOOK, _: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if object != OBJID_CLIENT.0 || !wanted() || GetAncestor(hwnd, GA_ROOT) != GetForegroundWindow() {
        return;
    }
    let mut acc = None;
    let mut id = VARIANT::default();
    if AccessibleObjectFromEvent(hwnd, object as u32, child as u32, &mut acc, &mut id).is_err() {
        return;
    }
    if let Some(acc) = acc {
        report(&acc, &id, 0);
    }
}

/// IAccessible2 attributes, which browsers implement. Asking for them is also
/// what turns accessibility on in Chromium.
unsafe fn attributes(acc: &IAccessible) -> Option<String> {
    let service = acc.cast::<IServiceProvider>().ok()?;
    let mut object: *mut c_void = std::ptr::null_mut();
    (Interface::vtable(&service).QueryService)(Interface::as_raw(&service), &IAccessible::IID, &IID_IACCESSIBLE2, &mut object)
        .ok()
        .ok()?;
    if object.is_null() {
        return None;
    }
    let object = IUnknown::from_raw(object);
    let table = *(object.as_raw() as *const *const *const c_void);
    let get_attributes: unsafe extern "system" fn(*mut c_void, *mut BSTR) -> HRESULT =
        std::mem::transmute(*table.add(IA2_GET_ATTRIBUTES));
    let mut text = BSTR::new();
    get_attributes(object.as_raw(), &mut text).is_ok().then(|| text.to_string())
}

/// What has focus inside `window`, waking its accessibility support if needed.
unsafe fn focused_in(window: HWND) -> Option<(IAccessible, VARIANT)> {
    let acc = client_object(window)?;
    let _ = attributes(&acc);
    let focus = acc.accFocus().ok()?;
    if let Ok(element) = IDispatch::try_from(&focus).and_then(|d| d.cast::<IAccessible>()) {
        return Some((element, VARIANT::from(0i32)));
    }
    i32::try_from(&focus).ok()?;
    Some((acc, focus))
}

/// Checks the focused control and answers `request`. Returns false if a
/// browser couldn't say what has focus yet. Until it can, the request stays
/// unanswered, so nothing is shown for text that might be a password.
unsafe fn check_focus(request: u64) -> bool {
    let foreground = GetForegroundWindow();
    if !wanted() || foreground.is_invalid() {
        send(FLAG_UNCHANGED, request);
        return true;
    }
    let mut pages: Vec<HWND> = Vec::new();
    let _ = EnumChildWindows(foreground, Some(collect_page), LPARAM(&mut pages as *mut Vec<HWND> as isize));
    if !pages.is_empty() {
        // the page first; the browser's own controls (address bar) after it
        for window in pages.iter().copied().chain([foreground]) {
            if let Some((element, id)) = focused_in(window) {
                report(&element, &id, request);
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
    match client_object(focused) {
        Some(acc) => report(&acc, &VARIANT::from(0i32), request),
        None => send(FLAG_UNCHANGED, request),
    }
    true
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

unsafe fn report(acc: &IAccessible, id: &VARIANT, request: u64) {
    let state = acc.get_accState(id).ok().and_then(|v| i32::try_from(&v).ok());
    let extra = attributes(acc);
    if state.is_none() && extra.is_none() {
        send(FLAG_UNCHANGED, request);
    } else {
        send(flags_for(state.unwrap_or(0), extra.as_deref()), request);
    }
}

unsafe fn send(flags: usize, request: u64) {
    let (target, message) = TARGET.with(Cell::get);
    let _ = PostMessageW(HWND(target as *mut c_void), message, WPARAM(flags), LPARAM(request as isize));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protected_state_means_password() {
        assert_eq!(flags_for(STATE_PROTECTED | 0x4, None), FLAG_PASSWORD);
        assert_eq!(flags_for(0x4, None), 0);
    }

    #[test]
    fn a_browser_input_type_also_means_password() {
        assert_eq!(flags_for(0, Some("tag:input;text-input-type:password")), FLAG_PASSWORD);
        assert_eq!(flags_for(0, Some("tag:input;text-input-type:text")), 0);
    }
}
