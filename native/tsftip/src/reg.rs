//! TSF 注册与注销（SilkRegister / SilkUnregister 的实现）。
//!
//! 两类注册（03）：COM in-proc server 写注册表，TIP profile/类别走 TSF 管理接口。
//! 全程幂等：重复注册不产生重复 profile，重复注销不报错。

use std::path::Path;

use windows::core::GUID;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use windows::Win32::UI::TextServices::{
    ITfCategoryMgr, ITfInputProcessorProfiles, CLSID_TF_CategoryMgr,
    CLSID_TF_InputProcessorProfiles, GUID_TFCAT_CATEGORY_OF_TIP, GUID_TFCAT_TIP_KEYBOARD,
};

use crate::util;

/// TIP 的 COM 类标识，与 InprocServer32 注册项一致。
pub const CLSID_SILK_TIP: GUID = GUID::from_u128(0x9d4e5f60_7a8b_4c9d_8e0f_1a2b3c4d5e6f);
/// 语言 profile 标识，与 AddLanguageProfile 写入项一致。
pub const GUID_SILK_PROFILE: GUID = GUID::from_u128(0x0e5f6a7b_8c9d_4e0f_a1b2_c3d4e5f6a7b8);
/// 简体中文。
pub const LANGID_ZH_CN: u16 = 0x0804;
/// 语言栏与键盘列表中显示的名称。
pub const PROFILE_NAME: &str = "Silk 拼音";
/// 图标取自 `silk-tsftip.dll` 的资源段（03:34 要求图标非空）。
///
/// 资源脚本还没有自带图标，索引 0 指向 DLL 的默认应用图标；加图标后只需改这里。
pub const PROFILE_ICON_INDEX: u32 = 0;

/// 注册作用域：user 全写 HKCU（无需管理员）；machine 只影响 COM 注册项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Machine,
}

impl Scope {
    fn parse(cmd: Option<&str>) -> Scope {
        match cmd.map(str::trim) {
            Some(c) if c.eq_ignore_ascii_case("machine") => Scope::Machine,
            _ => Scope::User,
        }
    }

    fn root(self) -> HKEY {
        match self {
            Scope::User => HKEY_CURRENT_USER,
            Scope::Machine => HKEY_LOCAL_MACHINE,
        }
    }
}

fn format_guid(clsid: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        clsid.data1,
        clsid.data2,
        clsid.data3,
        clsid.data4[0],
        clsid.data4[1],
        clsid.data4[2],
        clsid.data4[3],
        clsid.data4[4],
        clsid.data4[5],
        clsid.data4[6],
        clsid.data4[7],
    )
}

fn clsid_key() -> String {
    format!(r"Software\Classes\CLSID\{}", format_guid(&CLSID_SILK_TIP))
}

/// COM in-proc server：CLSID → InprocServer32 → DLL 路径 + Apartment。
fn register_com(scope: Scope, dll: &Path) -> bool {
    let inproc = format!("{}\\InprocServer32", clsid_key());
    let dll = dll.to_string_lossy();
    util::write_string(scope.root(), &inproc, None, &dll)
        && util::write_string(scope.root(), &inproc, Some("ThreadingModel"), "Apartment")
}

fn unregister_com(scope: Scope) -> bool {
    util::delete_tree(scope.root(), &clsid_key())
}

/// rundll32 调进来时线程尚未初始化 COM；S_FALSE 与 RPC_E_CHANGED_MODE 都可用。
struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    fn enter() -> ComGuard {
        // SAFETY: rundll32 线程可能已初始化 COM（S_FALSE）或处于别的套间模式
        // （RPC_E_CHANGED_MODE）；两种可接受情形都不需要配对 CoUninitialize。
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComGuard {
            uninit: hr.is_ok(),
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: 仅当 CoInitializeEx 返回 S_OK（本对象初始化了套间）才配对调用。
            unsafe { CoUninitialize() };
        }
    }
}

fn profiles() -> Option<ITfInputProcessorProfiles> {
    // SAFETY: CoCreateInstance 只返回新接口指针；参数是静态 CLSID，进程内服务器
    // 不需要聚合。
    unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }.ok()
}

fn categories() -> Option<ITfCategoryMgr> {
    // SAFETY: 同上，静态 CLSID + 进程内服务器。
    unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER) }.ok()
}

/// 确保 profile 存在并启用进当前用户键盘列表。
///
/// `AddLanguageProfile` 对已存在的 profile 会失败，这不算错（幂等），真正的
/// 失败会在后面的 `EnableLanguageProfile` 暴露。
fn register_profile() -> bool {
    let Some(profiles) = profiles() else {
        return false;
    };
    let name = util::utf16(PROFILE_NAME);
    // 图标取自本 TIP 的 DLL 资源段（03:34）：pchIconFile 必须是可定位的路径，
    // uIconIndex 是资源 id；给裸文件名时 TSF 不保证解析，所以用模块全路径。
    let icon_file = util::utf16(&util::module_path().unwrap_or_default().to_string_lossy());
    // SAFETY: profiles 有效；两个字符串参数按「指针 + 字符数」传（AddLanguageProfile
    // 的 cchDesc/cchIconFile），缓冲在本函数内独占且覆盖整个调用；GUID/CLSID 为静态
    // 常量。返回值按幂等处理。
    unsafe {
        let _ = profiles.Register(&CLSID_SILK_TIP);
        // 已存在时失败即幂等成功：真正的问题是随后 EnableLanguageProfile 能否成功。
        let _ = profiles.AddLanguageProfile(
            &CLSID_SILK_TIP,
            LANGID_ZH_CN,
            &GUID_SILK_PROFILE,
            &name,
            &icon_file,
            PROFILE_ICON_INDEX,
        );
        profiles
            .EnableLanguageProfile(&CLSID_SILK_TIP, LANGID_ZH_CN, &GUID_SILK_PROFILE, true)
            .is_ok()
    }
}

fn register_categories() -> bool {
    let Some(mgr) = categories() else {
        return false;
    };
    let mut ok = true;
    // SAFETY: mgr 有效；CLSID/GUID 均为静态常量，RegisterCategory 只读它们。
    unsafe {
        for catid in [GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_CATEGORY_OF_TIP] {
            ok &= mgr
                .RegisterCategory(&CLSID_SILK_TIP, &catid, &CLSID_SILK_TIP)
                .is_ok();
        }
    }
    ok
}

/// 注册全部内容，并写 host 懒启动用的安装目录（DLL 所在目录）。
pub fn register(scope: Scope) -> bool {
    let _guard = ComGuard::enter();
    let Some(dir) = util::module_dir() else {
        return false;
    };
    register_into(scope, &dir)
}

fn register_into(scope: Scope, dir: &Path) -> bool {
    // 用真实模块路径而不是拼文件名：cargo 产物名与安装名可能不同（见 README 的改名步骤）。
    let dll = util::module_path().unwrap_or_else(|| dir.join("silk-tsftip.dll"));
    let com = register_com(scope, &dll);
    let profile = register_profile();
    let cats = register_categories() && crate::ui::register_display_attribute();
    let install = util::write_install_path(dir);
    com && profile && cats && install
}

/// 逆序清理：类别 → profile → COM → 安装目录。
pub fn unregister(scope: Scope) -> bool {
    let _guard = ComGuard::enter();
    if let Some(mgr) = categories() {
        // SAFETY: mgr 有效；撤销「项不存在」属于幂等成功，与注册时的失败语义对称
        // （03:46：重复注销不报错），因此这里不计入 ok。
        unsafe {
            for catid in [GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_CATEGORY_OF_TIP] {
                let _ = mgr.UnregisterCategory(&CLSID_SILK_TIP, &catid, &CLSID_SILK_TIP);
            }
        }
    }
    if let Some(profiles) = profiles() {
        // SAFETY: 同上，两类撤销都按幂等处理：不存在的 profile 撤销失败不是错误。
        unsafe {
            let _ = profiles.RemoveLanguageProfile(
                &CLSID_SILK_TIP,
                LANGID_ZH_CN,
                &GUID_SILK_PROFILE,
            );
            let _ = profiles.Unregister(&CLSID_SILK_TIP);
        }
    }
    let mut ok = unregister_com(scope);
    // 安装目录值是 per-user 的懒启动输入，只有 user 作用域注销时才清理；
    // Machine 注销不得动 HKCU（那是另一个用户态账户的注册）。
    if scope == Scope::User {
        ok &= util::delete_tree(HKEY_CURRENT_USER, r"Software\Silk");
    }
    ok
}

/// 安装器提升上下文里注册完后，以原用户身份补登记键盘列表（03 的踩坑项）。
pub fn register_self() -> bool {
    let _guard = ComGuard::enter();
    let Some(profiles) = profiles() else {
        return false;
    };
    // SAFETY: profiles 有效；CLSID/GUID 为静态常量，本调用只改用户键盘列表状态。
    unsafe {
        profiles
            .EnableLanguageProfile(&CLSID_SILK_TIP, LANGID_ZH_CN, &GUID_SILK_PROFILE, true)
            .is_ok()
    }
}

/// rundll32 的命令行尾串：NUL 结尾的 ANSI 文本，可能为空。
///
/// # Safety
/// `cmd` 必须是 rundll32 提供的可读 NUL 结尾串或空指针。
pub unsafe fn cmd_arg(cmd: *const u8) -> Option<String> {
    if cmd.is_null() {
        return None;
    }
    let mut len = 0;
    // SAFETY: 调用方保证 cmd 是 NUL 结尾的可读串；逐个字节找终止符不会越界。
    while unsafe { *cmd.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: 上面已走到 NUL，前 len 个字节都在同一块可读内存内；from_utf8_lossy
    // 只读这段切片，不保留引用。
    let text = String::from_utf8_lossy(unsafe { core::slice::from_raw_parts(cmd, len) })
        .trim()
        .to_owned();
    (!text.is_empty()).then_some(text)
}

/// `SilkRegister` 的实现体。
pub fn run_register(cmd: Option<&str>) {
    let _ = register(Scope::parse(cmd));
}

/// `SilkUnregister` 的实现体。
pub fn run_unregister(cmd: Option<&str>) {
    let _ = unregister(Scope::parse(cmd));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parsing_is_forgiving() {
        assert_eq!(Scope::parse(None), Scope::User);
        assert_eq!(Scope::parse(Some("")), Scope::User);
        assert_eq!(Scope::parse(Some("user")), Scope::User);
        assert_eq!(Scope::parse(Some("Machine")), Scope::Machine);
        assert_eq!(Scope::parse(Some(" machine ")), Scope::Machine);
    }

    #[test]
    fn clsid_key_is_braced_guid() {
        let key = clsid_key();
        assert!(key.starts_with(r"Software\Classes\CLSID\{"));
        assert!(key.contains("9D4E5F60-7A8B-4C9D-8E0F-1A2B3C4D5E6F"));
        assert_eq!(
            format_guid(&CLSID_SILK_TIP),
            "{9D4E5F60-7A8B-4C9D-8E0F-1A2B3C4D5E6F}"
        );
    }

    #[test]
    fn scope_roots_differ() {
        assert_eq!(Scope::User.root(), HKEY_CURRENT_USER);
        assert_eq!(Scope::Machine.root(), HKEY_LOCAL_MACHINE);
    }

    #[test]
    fn cmd_arg_reads_ansi_tail() {
        let text = b"machine\0";
        // SAFETY: 三个输入分别是 NUL 结尾字节串、空指针与只含 NUL 的串，都在
        // cmd_arg 的文档契约内。
        let arg = unsafe { cmd_arg(text.as_ptr()) };
        assert_eq!(arg.as_deref(), Some("machine"));
        // SAFETY: 空指针是 cmd_arg 明确接受的输入。
        assert_eq!(unsafe { cmd_arg(core::ptr::null()) }, None);
        let empty = b"\0";
        // SAFETY: 只含 NUL 的字节串，同样是合法输入。
        assert_eq!(unsafe { cmd_arg(empty.as_ptr()) }, None);
    }
}
