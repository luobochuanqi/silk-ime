//! TSF 注册与注销（SilkRegister / SilkUnregister 的实现）。
//!
//! 两类注册（03）：COM in-proc server 写注册表，TIP profile/类别走 TSF 管理接口。
//! 全程幂等：重复注册不产生重复 profile，重复注销不报错。

use std::path::Path;

use windows::core::{w, GUID, Interface, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    ITfCategoryMgr, ITfInputProcessorProfileMgr, ITfInputProcessorProfiles, CLSID_TF_CategoryMgr,
    CLSID_TF_InputProcessorProfiles, GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_COMLESS,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};

use crate::util;

/// TIP 的 COM 类标识，与 InprocServer32 注册项一致。
pub const CLSID_SILK_TIP: GUID = GUID::from_u128(0x9d4e5f60_7a8b_4c9d_8e0f_1a2b3c4d5e6f);
/// 语言 profile 标识，与 AddLanguageProfile 写入项一致。
pub const GUID_SILK_PROFILE: GUID = GUID::from_u128(0x0e5f6a7b_8c9d_4e0f_a1b2_c3d4e5f6a7b8);
/// 简体中文。
pub const LANGID_ZH_CN: u16 = 0x0804;
/// 语言栏与键盘列表中显示的名称。
pub const PROFILE_NAME: &str = "Silk 拼音";
/// 图标索引：取 `shell32.dll` 的首个系统图标；自绘图标落地后改回本 DLL 资源段（03:34）。
pub const PROFILE_ICON_INDEX: u32 = 0;

/// 注册层级：machine = 机器级（COM + TIP + 类别，写 HKLM，需提权）；
/// user = 用户级（启用 + 用户语言列表，写 HKCU，普通权限）。
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

/// 机器级 profile 注册（写 HKLM，需提权）：一次写全 LanguageProfile 各值。
///
/// 用 Vista+ 的 `ITfInputProcessorProfileMgr::RegisterProfile`（官方推荐，配方 §7.1-3）；
/// bEnabledByDefault=false，启用状态由用户级 `EnableLanguageProfile` 决定。
fn register_profile() -> bool {
    let Some(profiles) = profiles() else {
        return false;
    };
    // 字符串以含终止符的容量传入：cch 按缓冲容量解释的实现会一直读到 NUL，
    // 无终止符的切片在页边界上会越界（真机出现过 MSCTF.dll 内的间歇性 AV）。
    let name = util::utf16z(PROFILE_NAME);
    // 本 DLL 暂无资源段，空资源索引取不到图标句柄；指向确定存在的系统图标文件。
    let icon_file = util::utf16z(&format!(
        r"{}\System32\shell32.dll",
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string())
    ));
    // SAFETY: profiles 有效；两个字符串缓冲独占且覆盖整个调用；GUID/CLSID 为静态
    // 常量；hklsubstitute 为空表示无替代布局。返回 false 由上层记日志并整体报错。
    unsafe {
        let Ok(mgr) = profiles.cast::<ITfInputProcessorProfileMgr>() else {
            return false;
        };
        mgr.RegisterProfile(
            &CLSID_SILK_TIP,
            LANGID_ZH_CN,
            &GUID_SILK_PROFILE,
            &name,
            &icon_file,
            PROFILE_ICON_INDEX,
            HKL(core::ptr::null_mut()),
            0,
            false,
            0,
        )
        .is_ok()
    }
}

/// 配方 §7.1-4 的类别集合：键盘类别决定「添加键盘」候选资格，TIPCAP 系列决定
/// 切换器、现代宿主、托盘等能力过滤；缺项会被过滤（五家实现全有）。
fn categories_list() -> [GUID; 6] {
    [
        GUID_TFCAT_TIP_KEYBOARD,
        GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
        GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
        GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
        GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
        GUID_TFCAT_TIPCAP_COMLESS,
    ]
}

fn register_categories() -> bool {
    let Some(mgr) = categories() else {
        return false;
    };
    let mut ok = true;
    // SAFETY: mgr 有效；CLSID/GUID 均为静态常量，RegisterCategory 只读它们。
    unsafe {
        for catid in categories_list() {
            ok &= mgr
                .RegisterCategory(&CLSID_SILK_TIP, &catid, &CLSID_SILK_TIP)
                .is_ok();
        }
    }
    ok
}

/// 机器级注册（必须提权）：COM + TIP profile + 类别，全部写 HKLM。
///
/// 这组 API 非提权一律 E_FAIL（配方 §1），任一步失败整体报错，不产出
/// 「看得见但激活失败」的幽灵条目（配方 §3）。rundll32 不自动提权。
pub fn register_machine() -> bool {
    let _guard = ComGuard::enter();
    let Some(dir) = util::module_dir() else {
        report(Scope::Machine, "register_machine", "module_dir 不可用", false);
        return false;
    };
    // 用真实模块路径而不是拼文件名：cargo 产物名与安装名可能不同（见 README 的改名步骤）。
    let dll = util::module_path().unwrap_or_else(|| dir.join("silk-tsftip.dll"));
    let com = register_com(Scope::Machine, &dll);
    let profile = register_profile();
    let cats = register_categories() && crate::ui::register_display_attribute();
    let ok = com && profile && cats;
    report(
        Scope::Machine,
        "register_machine",
        &format!("dir={} com={com} profile={profile} cats={cats}", dir.display()),
        ok,
    );
    ok
}

/// 用户级注册（普通权限）：启用 profile、登记进用户语言列表、写 host 懒启动目录。
///
/// 用户语言列表成员资格决定 Win+空格 的可见性（配方 §2.2）。
pub fn register_user() -> bool {
    let _guard = ComGuard::enter();
    let Some(profiles) = profiles() else {
        report(Scope::User, "register_user", "ITfInputProcessorProfiles 不可用", false);
        return false;
    };
    // SAFETY: profiles 有效；GUID/CLSID 为静态常量，本调用只改用户级启用位。
    let enable = unsafe {
        profiles
            .EnableLanguageProfile(&CLSID_SILK_TIP, LANGID_ZH_CN, &GUID_SILK_PROFILE, true)
    }
    .is_ok();
    let list = add_user_language_list();
    let install = util::module_dir().is_some_and(|dir| util::write_install_path(&dir));
    let ok = enable && list && install;
    report(
        Scope::User,
        "register_user",
        &format!("enable={enable} list={list} install={install}"),
        ok,
    );
    ok
}

/// 用户语言列表子键：产品当前范围是简体中文（0x0804）。
const USER_LIST_SUBKEY: &str = r"Control Panel\International\User Profile\zh-Hans-CN";

/// 值名格式 `0804:<CLSID><Profile>`：两段带花括号的 GUID 紧邻（系统真实形态）。
fn user_list_value_name() -> String {
    format!(
        "0804:{}{}",
        format_guid(&CLSID_SILK_TIP),
        format_guid(&GUID_SILK_PROFILE)
    )
}

/// 登记进用户语言列表。序号必须是 1 起连续，空洞会让 InputSwitch.dll 崩
///（PIME 注释，配方 §7.1-5）；已连续时保持原序不动。
fn add_user_language_list() -> bool {
    let name = user_list_value_name();
    let mut entries = util::enum_dword_values(HKEY_CURRENT_USER, USER_LIST_SUBKEY);
    if !entries.iter().any(|(n, _)| *n == name) {
        let next = entries.iter().map(|(_, v)| *v).max().unwrap_or(0) + 1;
        entries.push((name, next));
    }
    write_user_list(&entries)
}

fn remove_user_language_list() -> bool {
    let name = user_list_value_name();
    let mut entries = util::enum_dword_values(HKEY_CURRENT_USER, USER_LIST_SUBKEY);
    let had = entries.iter().any(|(n, _)| *n == name);
    entries.retain(|(n, _)| *n != name);
    let ok = if had {
        util::delete_value(HKEY_CURRENT_USER, USER_LIST_SUBKEY, &name)
    } else {
        true
    };
    write_user_list(&entries) && ok
}

fn write_user_list(entries: &[(String, u32)]) -> bool {
    let mut sorted: Vec<&(String, u32)> = entries.iter().collect();
    sorted.sort_by_key(|(_, v)| *v);
    let contiguous = sorted.iter().enumerate().all(|(i, (_, v))| *v == i as u32 + 1);
    let mut ok = true;
    for (i, (name, value)) in sorted.iter().enumerate() {
        let data = if contiguous { *value } else { i as u32 + 1 };
        ok &= util::write_dword(HKEY_CURRENT_USER, USER_LIST_SUBKEY, name, data);
    }
    ok
}

/// 机器级反注册（提权）：类别 → profile → COM 逆序清理，幂等。
pub fn unregister_machine() -> bool {
    let _guard = ComGuard::enter();
    if let Some(mgr) = categories() {
        // SAFETY: mgr 有效；撤销「项不存在」属于幂等成功，与注册时的失败语义对称
        // （03:46：重复注销不报错），因此这里不计入 ok。
        unsafe {
            for catid in categories_list() {
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
    let mut ok = unregister_com(Scope::Machine);
    // 历史版本写过按用户的 COM 注册，一并清理。
    ok &= util::delete_tree(HKEY_CURRENT_USER, &clsid_key());
    report(Scope::Machine, "unregister_machine", "", ok);
    ok
}

/// 用户级反注册（普通权限）：移出用户语言列表并压实序号、停用 profile、清理安装目录值。
pub fn unregister_user() -> bool {
    let _guard = ComGuard::enter();
    let list = remove_user_language_list();
    if let Some(profiles) = profiles() {
        // SAFETY: 同 register_user；不存在的 profile 停用失败按幂等处理。
        let _ = unsafe {
            profiles.EnableLanguageProfile(&CLSID_SILK_TIP, LANGID_ZH_CN, &GUID_SILK_PROFILE, false)
        };
    }
    let install = util::delete_tree(HKEY_CURRENT_USER, r"Software\Silk");
    let ok = list && install;
    report(Scope::User, "unregister_user", &format!("list={list} install={install}"), ok);
    ok
}

/// rundll32 入口没有控制台：结果写入 silkreg.log，失败另弹窗，避免静默失败不可诊断。
fn report(scope: Scope, action: &str, detail: &str, ok: bool) {
    use std::io::Write;
    let line = format!("{action} scope={scope:?} ok={ok} {detail}");
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        let dir = Path::new(&dir).join("Silk").join("logs");
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(mut f) =
                std::fs::OpenOptions::new().create(true).append(true).open(dir.join("silkreg.log"))
            {
                let _ = writeln!(f, "{line}");
            }
        }
    }
    if !ok {
        let text = util::utf16z(&line);
        // SAFETY: 文本 NUL 结尾且存活到调用结束；无属主窗口，MB_OK 等用户确认。
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Silk 注册"), MB_OK | MB_ICONWARNING);
        }
    }
}

/// 安装器提升上下文里注册完后，以原用户身份补登记键盘列表（03 的踩坑项）。
pub fn register_self() -> bool {
    register_user()
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

/// `SilkRegister` 的实现体：machine = 机器级（提权），user = 用户级（普通权限）。
pub fn run_register(cmd: Option<&str>) {
    match Scope::parse(cmd) {
        Scope::Machine => {
            let _ = register_machine();
        }
        Scope::User => {
            let _ = register_user();
        }
    }
}

/// `SilkUnregister` 的实现体，与注册的层级语义对称。
pub fn run_unregister(cmd: Option<&str>) {
    match Scope::parse(cmd) {
        Scope::Machine => {
            let _ = unregister_machine();
        }
        Scope::User => {
            let _ = unregister_user();
        }
    }
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
