//! TypeFlow setup: one exe that carries the app and installs it for the
//! current user, with nothing else to download or install.
//!
//! - Installs to `%LOCALAPPDATA%\Programs\TypeFlow\` (no admin needed).
//! - Adds a Start menu shortcut and an entry in Settings › Apps for uninstalling.
//! - Offers to add the English and Persian keyboards if either is missing.
//! - Starts TypeFlow, which adds itself to Start with Windows on first run.
//!
//! `TypeFlow-Setup.exe /S` installs without asking anything (and doesn't
//! touch the keyboards). The copy installed as `uninstall.exe` removes it:
//! `uninstall.exe /uninstall`, with `/S` for no questions. Settings in
//! `%APPDATA%\TypeFlow\` are kept.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayoutList, HKL};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static APP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/typeflow.exe"));
const VERSION: &str = env!("TYPEFLOW_VERSION");
const NAME: &str = "TypeFlow";
const PUBLISHER: &str = "Sam Adeli";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\TypeFlow";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_lowercase()).collect();
    let quiet = args.iter().any(|a| a == "/s" || a == "/silent");
    let result = if args.iter().any(|a| a == "/uninstall") { uninstall(quiet) } else { install(quiet) };
    if let Err(e) = result {
        if !quiet {
            message(&format!("{e}"), MB_ICONERROR);
        }
        std::process::exit(1);
    }
}

// ------------------------------------------------------------------- paths ---

fn env_dir(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name).map(PathBuf::from).ok_or_else(|| format!("%{name}% is not set."))
}

fn install_dir() -> Result<PathBuf, String> {
    Ok(env_dir("LOCALAPPDATA")?.join("Programs").join(NAME))
}

fn shortcut_path() -> Result<PathBuf, String> {
    Ok(env_dir("APPDATA")?.join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{NAME}.lnk")))
}

// -------------------------------------------------------------- dialogs ---

fn message(text: &str, icon: MESSAGEBOX_STYLE) {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(format!("{NAME} Setup")), MB_OK | icon) };
}

fn ask(text: &str) -> bool {
    let answer = unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(format!("{NAME} Setup")), MB_YESNO | MB_ICONQUESTION)
    };
    answer == IDYES
}

// ------------------------------------------------------------- install ---

fn install(quiet: bool) -> Result<(), String> {
    let dir = install_dir()?;
    let exe = dir.join("typeflow.exe");
    let updating = exe.exists();
    if !quiet {
        let what = if updating { "Update" } else { "Install" };
        let text = format!(
            "{what} {NAME} {VERSION}?\n\n{NAME} shows a hint when you type a word with the wrong \
             keyboard layout (English ⇄ Persian).\n\nIt will be installed for you only, in\n{}\n\
             and will start with Windows. No admin rights are needed.",
            dir.display()
        );
        if !ask(&text) {
            return Ok(());
        }
        keyboards();
    }

    close_running();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}:\n{e}", dir.display()))?;
    write_retrying(&exe, APP)?;

    // a copy of this setup becomes the uninstaller
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstaller = dir.join("uninstall.exe");
    if !same_file(&me, &uninstaller) {
        let bytes = std::fs::read(&me).map_err(|e| format!("Could not read the setup file:\n{e}"))?;
        write_retrying(&uninstaller, &bytes)?;
    }

    shortcut(&exe, &shortcut_path()?)?;
    register(&dir, &exe, &uninstaller)?;

    Command::new(&exe).current_dir(&dir).spawn().map_err(|e| format!("{NAME} was installed but did not start:\n{e}"))?;
    if !quiet {
        message(
            &format!(
                "{NAME} {VERSION} is installed and running.\n\nLook for its icon near the clock (it may be \
                 under the ^ arrow at first). Left click opens the TypeFlow window; right click opens the menu."
            ),
            MB_ICONINFORMATION,
        );
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Writes a file, retrying for a few seconds while a closing copy of the
/// app still holds it.
fn write_retrying(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let start = Instant::now();
    loop {
        match std::fs::write(path, bytes) {
            Ok(()) => return Ok(()),
            Err(e) if start.elapsed() > Duration::from_secs(5) => {
                return Err(format!("Could not write {}:\n{e}\n\nIf {NAME} is running, exit it from its tray icon and try again.", path.display()))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Asks a running TypeFlow to exit (the same as Exit in its tray menu) and
/// waits for it to go.
fn close_running() {
    unsafe {
        let request = RegisterWindowMessageW(w!("TypeFlow.Exit"));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            let Ok(window) = FindWindowW(w!("TypeFlowBubble"), w!("TypeFlow")) else { break };
            if window.is_invalid() {
                break;
            }
            let _ = PostMessageW(window, request, WPARAM(0), LPARAM(0));
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

fn shortcut(target: &Path, link: &Path) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let made = (|| -> windows::core::Result<()> {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(dir) = target.parent() {
                shell_link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            shell_link.SetDescription(w!("Hints when you type with the wrong keyboard layout"))?;
            shell_link.cast::<IPersistFile>()?.Save(&HSTRING::from(link.as_os_str()), true)
        })();
        made.map_err(|e| format!("Could not create the Start menu shortcut:\n{e}"))
    }
}

// ------------------------------------------------------------ registry ---

fn reg_string(key: &str, name: &str, value: &str) -> Result<(), String> {
    let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name), REG_SZ.0,
            Some(data.as_ptr() as *const _), (data.len() * 2) as u32)
    };
    if r == ERROR_SUCCESS { Ok(()) } else { Err(format!("Could not write the registry ({name}): {}", r.0)) }
}

fn reg_dword(key: &str, name: &str, value: u32) -> Result<(), String> {
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name), REG_DWORD.0,
            Some(&value as *const u32 as *const _), 4)
    };
    if r == ERROR_SUCCESS { Ok(()) } else { Err(format!("Could not write the registry ({name}): {}", r.0)) }
}

/// The entry in Settings › Apps › Installed apps.
fn register(dir: &Path, exe: &Path, uninstaller: &Path) -> Result<(), String> {
    let k = UNINSTALL_KEY;
    reg_string(k, "DisplayName", NAME)?;
    reg_string(k, "DisplayVersion", VERSION)?;
    reg_string(k, "Publisher", PUBLISHER)?;
    reg_string(k, "DisplayIcon", &exe.display().to_string())?;
    reg_string(k, "InstallLocation", &dir.display().to_string())?;
    reg_string(k, "UninstallString", &format!("\"{}\" /uninstall", uninstaller.display()))?;
    reg_string(k, "QuietUninstallString", &format!("\"{}\" /uninstall /S", uninstaller.display()))?;
    reg_dword(k, "NoModify", 1)?;
    reg_dword(k, "NoRepair", 1)?;
    reg_dword(k, "EstimatedSize", ((APP.len() * 2) / 1024) as u32)?;
    Ok(())
}

// ----------------------------------------------------------- keyboards ---

/// (English installed, Persian installed)
fn layouts() -> (bool, bool) {
    unsafe {
        let n = GetKeyboardLayoutList(None);
        let mut list = vec![Default::default(); n.max(0) as usize];
        GetKeyboardLayoutList(Some(&mut list));
        let has = |lang: usize| list.iter().any(|h: &HKL| (h.0 as usize) & 0xFF == lang);
        (has(0x09), has(0x29))
    }
}

/// TypeFlow needs both keyboards. Offers to add whichever is missing, the
/// same way Settings › Language & region does.
fn keyboards() {
    let (en, fa) = layouts();
    if en && fa {
        return;
    }
    let missing = match (en, fa) {
        (false, false) => "English and Persian keyboards",
        (false, true) => "English keyboard",
        _ => "Persian keyboard",
    };
    let text = format!(
        "{NAME} works between the English and Persian keyboards, and the {missing} \
         {} not installed.\n\nAdd it now? (Same as adding the language in Settings › Time & language.)",
        if en || fa { "is" } else { "are" }
    );
    if !ask(&text) {
        return;
    }
    let script = "$l = Get-WinUserLanguageList; \
        if (-not ($l | Where-Object LanguageTag -like 'en*')) { $l.Add('en-US') }; \
        if (-not ($l | Where-Object LanguageTag -like 'fa*')) { $l.Add('fa-IR') }; \
        Set-WinUserLanguageList $l -Force";
    let ok = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let (en, fa) = layouts();
    if !ok || !(en && fa) {
        message(
            &format!(
                "The keyboard could not be added automatically. Add it in Settings › Time & language › \
                 Language & region, then {NAME} will pick it up the next time it starts."
            ),
            MB_ICONWARNING,
        );
    }
}

// ----------------------------------------------------------- uninstall ---

fn uninstall(quiet: bool) -> Result<(), String> {
    if !quiet && !ask(&format!("Remove {NAME} from this computer?\n\nYour settings are kept.")) {
        return Ok(());
    }
    let dir = install_dir()?;
    close_running();
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(NAME));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
    }
    if let Ok(link) = shortcut_path() {
        let _ = std::fs::remove_file(link);
    }
    let exe = dir.join("typeflow.exe");
    if exe.exists() {
        let start = Instant::now();
        while std::fs::remove_file(&exe).is_err() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    // This uninstaller is running from that folder, so a short-lived hidden
    // cmd removes the folder once it has exited.
    let _ = Command::new("cmd.exe")
        .raw_arg(format!("/c ping 127.0.0.1 -n 3 >nul & rmdir /s /q \"{}\"", dir.display()))
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    if !quiet {
        message(&format!("{NAME} was removed."), MB_ICONINFORMATION);
    }
    Ok(())
}
