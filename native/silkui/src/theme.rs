//! 主题：深色/浅色与高对比度配色。
//!
//! 配色在每次主题变化时重算并缓存进 [`Theme`]；DWM 边框色与背景色同源。

use windows::Win32::Graphics::Gdi::{GetSysColor, COLOR_WINDOW, COLOR_WINDOWTEXT};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETHIGHCONTRAST,
};

/// 直通 D2D 的 RGBA（sRGB，0..=1）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// 由 `0xRRGGBB` 构造不透明颜色。
    pub const fn rgb(v: u32) -> Self {
        Self {
            r: ((v >> 16) & 0xFF) as f32 / 255.0,
            g: ((v >> 8) & 0xFF) as f32 / 255.0,
            b: (v & 0xFF) as f32 / 255.0,
            a: 1.0,
        }
    }

    /// 由 `COLORREF`（0x00BBGGRR）构造不透明颜色。
    pub const fn from_colorref(v: u32) -> Self {
        Self::rgb(((v & 0xFF) << 16) | (v & 0xFF00) | ((v >> 16) & 0xFF))
    }

    /// `COLORREF`（0x00BBGGRR，注意与 `0xRRGGBB` 字节序相反），
    /// 用于 `DWMWA_BORDER_COLOR`。
    pub const fn to_colorref(self) -> u32 {
        let r = (self.r * 255.0) as u32;
        let g = (self.g * 255.0) as u32;
        let b = (self.b * 255.0) as u32;
        (b << 16) | (g << 8) | r
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub high_contrast: bool,
    pub background: Color,
    pub text: Color,
    pub comment: Color,
    pub border: Color,
    /// 底边分隔线的颜色（组字串与候选之间）。
    pub separator: Color,
}

const DARK_BACKGROUND: u32 = 0x202020;
const DARK_TEXT: u32 = 0xF5F5F5;
const DARK_COMMENT: u32 = 0x9A9A9A;
const DARK_BORDER: u32 = 0x3C3C3C;

const LIGHT_BACKGROUND: u32 = 0xFFFFFF;
const LIGHT_TEXT: u32 = 0x1B1B1B;
const LIGHT_COMMENT: u32 = 0x6B6B6B;
const LIGHT_BORDER: u32 = 0xD6D6D6;

impl Theme {
    pub const fn light() -> Self {
        Self {
            dark: false,
            high_contrast: false,
            background: Color::rgb(LIGHT_BACKGROUND),
            text: Color::rgb(LIGHT_TEXT),
            comment: Color::rgb(LIGHT_COMMENT),
            border: Color::rgb(LIGHT_BORDER),
            separator: Color::rgb(0xEDEDED),
        }
    }

    pub const fn dark() -> Self {
        Self {
            dark: true,
            high_contrast: false,
            background: Color::rgb(DARK_BACKGROUND),
            text: Color::rgb(DARK_TEXT),
            comment: Color::rgb(DARK_COMMENT),
            border: Color::rgb(DARK_BORDER),
            separator: Color::rgb(0x343434),
        }
    }

    /// 高对比度：完全跟随系统配色，禁用自定义色（06 无障碍）。
    pub fn high_contrast_theme() -> Self {
        // SAFETY: 无参数、无所有权转移的只读系统查询。
        let background = unsafe { Color::from_colorref(GetSysColor(COLOR_WINDOW)) };
        // SAFETY: 同上。
        let text = unsafe { Color::from_colorref(GetSysColor(COLOR_WINDOWTEXT)) };
        Self {
            dark: luminance(text) > luminance(background),
            high_contrast: true,
            background,
            text,
            comment: text,
            border: text,
            separator: text.with_alpha(0.35),
        }
    }

    /// 读取当前系统主题。注册表读不到（非 Win10+ 或键被禁）时按浅色处理。
    pub fn detect() -> Self {
        if high_contrast_enabled() {
            return Self::high_contrast_theme();
        }
        if apps_use_light_theme() == Some(false) {
            Self::dark()
        } else {
            Self::light()
        }
    }
}

fn luminance(c: Color) -> f32 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

fn high_contrast_enabled() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: core::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: `hc` 是正确大小、可写的 HIGHCONTRASTW 缓冲区。
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some(&mut hc as *mut _ as *mut core::ffi::c_void),
            Default::default(),
        )
    };
    ok.is_ok() && hc.dwFlags.contains(HCF_HIGHCONTRASTON)
}

fn apps_use_light_theme() -> Option<bool> {
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
    };
    use windows::core::{PCWSTR, PWSTR};

    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut data: u32 = 1;
    let mut size = core::mem::size_of::<u32>() as u32;
    // SAFETY: 两个字符串均以 NUL 结尾且存活于调用期间；`data`/`size` 与 REG_DWORD 匹配。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    // PWSTR 仅为签名占位，未使用。
    let _ = PWSTR::null();
    if status.is_ok() {
        Some(data != 0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_roundtrip() {
        // COLORREF 是 0x00BBGGRR：R 在低字节。
        let c = Color::from_colorref(0x00123456);
        assert_eq!(c.r, 0x56 as f32 / 255.0);
        assert_eq!(c.b, 0x12 as f32 / 255.0);
        assert_eq!(c.to_colorref(), 0x00123456);
        assert_eq!(Color::rgb(0x563412).to_colorref(), 0x00123456);
    }

    #[test]
    fn luminance_orders_black_white() {
        assert!(luminance(Color::rgb(0xFFFFFF)) > luminance(Color::rgb(0x000000)));
        assert!(luminance(Theme::dark().text) > luminance(Theme::dark().background));
        assert!(luminance(Theme::light().text) < luminance(Theme::light().background));
    }
}
