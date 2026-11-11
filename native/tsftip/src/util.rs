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
    // SAFETY: buf 是 512 个 u16 的独占缓冲（1024 字节），size 初值等于其字节容量，
    // 内核最多写这么多；key/value 都是静态 NUL 结尾串。
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
    // SAFETY: u16 对齐比 u8 更严格，把同一块内存重解释为字节切片只放宽对齐要求；
    // 长度按 2 字节/元素计，value 在调用期间存活且不被修改。
    let bytes = unsafe {
        core::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2)
    };
    // SAFETY: 同 write_install_path：bytes 指向存活的 UTF-16 缓冲，REG_SZ 按字节计数。
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
    let bytes = {
        // SAFETY: 同 write_install_path：u16 → u8 只放宽对齐，长度按字节计；
        // data 在调用结束前一直存活且不被修改。
        unsafe { core::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) }
    };
    // SAFETY: 三个缓冲都存活到调用结束；namez 为 None 时取 PCWSTR::null（默认值）。
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
    // SAFETY: sub 是 NUL 结尾的 UTF-16 且存活到调用结束；root 由调用方按作用域给。
    let status = unsafe { RegDeleteTreeW(root, pcwstr(&sub)) };
    status.is_ok() || status == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
}

/// 当前进程用户 SID 的字符串形式，管道名与 ACL 都用它。
pub fn current_user_sid() -> Option<String> {
    let mut token = Default::default();
    // SAFETY: 取当前进程的令牌句柄；token 是本栈上的可写量，TOKEN_QUERY 只要求查询权限。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    // 句柄一旦取到就必须关闭，所以统一在出口前处理，失败路径也不例外。
    let sid = query_token_sid(token);
    // SAFETY: token 由上面的 OpenProcessToken 独占取得，这里关闭是唯一释放点。
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(token);
    }
    sid
}

/// 读令牌里的用户 SID 并转成字符串；`token` 由调用方打开与关闭。
fn query_token_sid(token: windows::Win32::Foundation::HANDLE) -> Option<String> {
    let mut size = 0u32;
    // SAFETY: 传 None 缓冲区是文档规定的「问需要多大」用法，只写 size。
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
    }
    if size == 0 {
        return None;
    }
    // TOKEN_USER 内含指针，按 usize 对齐分配即可容纳内核写入的整个结构。
    let words = (size as usize).div_ceil(8);
    let mut buffer = vec![0usize; words];
    // SAFETY: buffer 有 size 字节可用（words 向上取整），token 有效；成功后 TOKEN_USER
    // 结构整体落在 buffer 内，其中的指针指向 buffer 自己的尾部。
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    };
    ok.ok()?;
    // SAFETY: 调用成功即保证 buffer 前 size 字节是内核填好的 TOKEN_USER。
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let mut text = PWSTR::null();
    // SAFETY: user.User.Sid 指向 buffer 内的有效 SID；text 是本栈可写量，成功后由
    // LocalAlloc 分配、下面负责释放。
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }.ok()?;
    // SAFETY: text 指向上面刚分配、且 NUL 结尾的 UTF-16 字符串。
    let sid = unsafe { text.to_string() }.ok();
    // SAFETY: text 由 ConvertSidToStringSidW 分配，本函数是唯一所有者。
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    sid
}

/// 本 DLL 的完整路径。
pub fn module_path() -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: buf 是 1024 个 u16 的独占缓冲，内核最多写这么多字符；None 表示本模块。
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
