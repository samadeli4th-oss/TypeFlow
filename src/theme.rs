//! Windows color settings (Settings › Personalization › Colors) and the
//! acrylic glass behind the hint bubble.
//!
//! - **Mode**: `SystemUsesLightTheme`, the mode of Start, the taskbar and
//!   system flyouts, which the bubble belongs with.
//! - **Accent**: `DWM\AccentColor`. It tints the glass only while "Show accent
//!   color on Start and taskbar" (`ColorPrevalence`) is on, like Start does.
//! - **Transparency effects** (`EnableTransparency`): off gives a solid fill.
//!
//! Everything is re-read each time the bubble is shown, so a change in
//! Settings applies to the next hint without a restart.

use std::ffi::c_void;

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::*;

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const DWM: PCWSTR = w!("Software\\Microsoft\\Windows\\DWM");

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub light: bool,
    pub accent_on_surfaces: bool,
    pub transparency: bool,
    /// (r, g, b)
    pub accent: (u8, u8, u8),
}

/// What the bubble paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub fill: (u8, u8, u8),
    /// 255 = solid (transparency effects off)
    pub fill_alpha: u8,
    pub text: (u8, u8, u8),
}

impl Theme {
    /// Windows 11 defaults: light mode, blue accent, glass on, accent off.
    pub const DEFAULT: Theme = Theme { light: true, accent_on_surfaces: false, transparency: true, accent: (0, 120, 212) };

    pub fn current() -> Theme {
        let d = Theme::DEFAULT;
        let flag = |key, name, default: bool| dword(key, name).map_or(default, |v| v != 0);
        Theme {
            light: flag(PERSONALIZE, w!("SystemUsesLightTheme"), d.light),
            accent_on_surfaces: flag(PERSONALIZE, w!("ColorPrevalence"), d.accent_on_surfaces),
            transparency: flag(PERSONALIZE, w!("EnableTransparency"), d.transparency),
            // stored as 0xAABBGGRR
            accent: dword(DWM, w!("AccentColor")).map_or(d.accent, |v| (v as u8, (v >> 8) as u8, (v >> 16) as u8)),
        }
    }

    pub fn colors(&self) -> Colors {
        let base = if self.light { 243.0 } else { 40.0 };
        let tint = match (self.accent_on_surfaces, self.light) {
            (false, _) => 0.0,
            (true, true) => 0.20,
            (true, false) => 0.38,
        };
        let mix = |a: u8| (base + (a as f64 - base) * tint).round() as u8;
        let (r, g, b) = self.accent;
        Colors {
            fill: (mix(r), mix(g), mix(b)),
            // 38% / 34%: see-through, with the blur doing the separating
            // solid: text over see-through glass rendered badly
            fill_alpha: 255,
            text: if self.light { (0x1B, 0x1B, 0x1B) } else { (0xFF, 0xFF, 0xFF) },
        }
    }
}

fn dword(key: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_DWORD, None,
            Some(&mut data as *mut u32 as *mut c_void), Some(&mut size))
    };
    (r == ERROR_SUCCESS).then_some(data)
}

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    /// 0xAABBGGRR
    gradient: u32,
    animation: u32,
}

#[repr(C)]
struct CompositionData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

type SetWindowCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL;

/// Rounded corners, dark/light frame, and the tinted acrylic behind `window`.
///
/// The acrylic comes from user32's `SetWindowCompositionAttribute`: unlike the
/// documented `DWMWA_SYSTEMBACKDROP_TYPE` it takes a tint color and stays on
/// while the window is inactive, which the bubble always is. If it's missing
/// the bubble is simply drawn solid.
pub fn apply(window: HWND, theme: &Theme) {
    let c = theme.colors();
    unsafe {
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(window, DWMWA_WINDOW_CORNER_PREFERENCE,
            &round as *const _ as *const c_void, std::mem::size_of_val(&round) as u32);
        let dark = BOOL::from(!theme.light);
        let _ = DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const c_void, std::mem::size_of_val(&dark) as u32);

        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
        let Some(f) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return };
        let set: SetWindowCompositionAttribute = std::mem::transmute(f);
        let (r, g, b) = c.fill;
        let mut policy = AccentPolicy {
            state: if c.fill_alpha < 255 { ACCENT_ENABLE_ACRYLICBLURBEHIND } else { ACCENT_DISABLED },
            flags: 2,
            gradient: (c.fill_alpha as u32) << 24 | (b as u32) << 16 | (g as u32) << 8 | r as u32,
            animation: 0,
        };
        let mut data = CompositionData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut _ as *mut c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        let _ = set(window, &mut data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_tints_only_when_shown_on_surfaces() {
        let neutral = Theme { accent_on_surfaces: false, ..Theme::DEFAULT }.colors();
        assert_eq!(neutral.fill, (243, 243, 243));
        let tinted = Theme { accent_on_surfaces: true, ..Theme::DEFAULT }.colors();
        assert_eq!(tinted.fill, (194, 218, 237));
    }

    #[test]
    fn bubble_is_always_solid() {
        let c = Theme { transparency: true, light: false, ..Theme::DEFAULT }.colors();
        assert_eq!(c.fill_alpha, 255);
        assert_eq!(c.text, (255, 255, 255));
    }
}
