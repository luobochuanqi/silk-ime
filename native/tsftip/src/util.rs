//! 平台小工具：UTF-16 转换、注册表读写、当前用户 SID、模块路径。

use std::path::{Path, PathBuf};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::System::Registry::HKEY;
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// 安装根目录：silk-tsftip.dll / silkhost.exe / silkui.dll / dict\ 同层（07）。
pub const INSTALL_KEY: PCWSTR = w!("Software\\Silk");
pub const INSTALL_VALUE: PCWSTR = w!("InstallPath");

pub fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 带 NUL 结尾的 UTF-16，供接收 TSF 字符串指针的 API 使用。
pub fn utf16z(text: &str) -> Vec<u16> {
    let mut out = utf16(text);
    out.push(0);
    out
}

pub fn pcwstr(text: &[u16]) -> PCWSTR {
    PCWSTR(text.as_ptr())
}

/// 读 `HKCU\Software\Silk` 下的 REG_SZ 值；缺失或非法返回 None。
pub fn read_install_path() -> Option<PathBuf> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            INSTALL_KEY,
            INSTALL_VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() || size < 2 {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..len]);
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}

/// 写 `HKCU\Software\Silk\InstallPath`，值为目录路径。
pub fn write_install_path(dir: &Path) -> bool {
    let value = utf16z(&dir.to_string_lossy());
    let bytes = unsafe {
        core::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2)
    };
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            INSTALL_KEY,
            INSTALL_VALUE,
            REG_SZ.0,
            Some(bytes.as_ptr().cast()),
            bytes.len() as u32,
        )
    };
    status.is_ok()
}

/// 写入字符串值，`key` 为完整子键路径。
pub fn write_string(root: HKEY, subkey: &str, name: Option<&str>, value: &str) -> bool {
    let sub = utf16z(subkey);
    let namez = name.map(utf16z);
    let data = utf16z(value);
    let bytes =
        unsafe { core::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
    let status = unsafe {
        RegSetKeyValueW(
            root,
            pcwstr(&sub),
            namez.as_deref().map(pcwstr).unwrap_or(PCWSTR::null()),
            REG_SZ.0,
            Some(bytes.as_ptr().cast()),
            bytes.len() as u32,
        )
    };
    status.is_ok()
}

/// 以 `HKLM\Software\Classes\...` 这类完整路径删除整棵子键；键不存在也算成功。
pub fn delete_tree(root: HKEY, subkey: &str) -> bool {
    use windows::Win32::System::Registry::RegDeleteTreeW;
    let sub = utf16z(subkey);
    let status = unsafe { RegDeleteTreeW(root, pcwstr(&sub)) };
    status.is_ok() || status == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
}

/// 当前进程用户 SID 的字符串形式，管道名与 ACL 都用它。
pub fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token = Default::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut size = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        if size == 0 {
            let _ = windows::Win32::Foundation::CloseHandle(token);
            return None;
        }
        let words = (size as usize).div_ceil(8);
        let mut buffer = vec![0usize; words];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok.ok()?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

/// 本 DLL 的完整路径。
pub fn module_path() -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let len = unsafe { GetModuleFileNameW(None, &mut buf) };
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// 本 DLL 所在目录；安装布局里它同时是 host 与词库的根。
pub fn module_dir() -> Option<PathBuf> {
    module_path().and_then(|path| path.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_helpers() {
        assert_eq!(utf16("ab"), vec![0x61, 0x62]);
        assert_eq!(utf16z("a"), vec![0x61, 0]);
        assert_eq!(utf16("中"), vec![0x4E2D]);
    }
}
