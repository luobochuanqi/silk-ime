//! Silk 输入法的 TSF 适配层（silk-tsftip.dll）。
//!
//! 职责只有四件：COM/TSF 对象、按键拦截、composition 维护、协议客户端。
//! 输入逻辑全部在 silkhost；本 DLL 不含词库与引擎依赖。契约见
//! docs/design/foundation/03-windows-platform.md 与 04-process-and-ipc.md。

mod client;
mod edit;
mod key;
mod logger;
mod msg;
mod reg;
mod session;
mod tip;
mod ui;
mod util;

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{GUID, Interface, IUnknown, Result, HRESULT};
use windows::core::BOOL;
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOINTERFACE, E_POINTER, HINSTANCE, HMODULE,
    HWND,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// 本 DLL 的模块句柄，供注册找自身路径。
static MODULE: AtomicIsize = AtomicIsize::new(0);

/// 在 DllMain 里只记录模块句柄；COM/图形初始化一律延后（03 的初始化时机约束）。
pub fn set_module(module: HMODULE) {
    MODULE.store(module.0 as isize, Ordering::Release);
}

pub fn module() -> Option<HMODULE> {
    let raw = MODULE.load(Ordering::Acquire);
    (raw != 0).then_some(HMODULE(raw as *mut core::ffi::c_void))
}

/// TIP 的 COM 类厂。
#[windows::core::implement(IClassFactory)]
pub struct Factory;

// COM 回调签名固定为 safe fn；空指针与归属由 DllGetClassObject 与本函数共同校验。
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: windows::core::Ref<IUnknown>,
        riid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        if object.is_null() || riid.is_null() {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        if !outer.is_null() {
            return Err(windows::core::Error::from_hresult(CLASS_E_NOAGGREGATION));
        }
        let unknown: IUnknown = tip::Tip::new().into();
        let mut out = core::ptr::null_mut();
        // SAFETY: riid 非空（上面判过）且由 COM 保证指向合法 IID；out 是本地可写量。
        let hr = unsafe { unknown.query(riid, &mut out) };
        if hr.is_err() {
            return Err(windows::core::Error::from_hresult(E_NOINTERFACE));
        }
        // SAFETY: object 非空（上面判过），按 COM 契约可写；out 是刚取得的接口指针。
        unsafe { *object = out };
        Ok(())
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// COM 入口。只支持本 DLL 的 CLSID，其余一律 CLASS_E_CLASSNOTAVAILABLE。
///
/// # Safety
/// 由系统 COM 调用，指针按 COM 契约可写。
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    object: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || object.is_null() {
        return E_POINTER;
    }
    // SAFETY: 三个指针由 COM 保证有效可写（已判空）；读 rclsid 与写 *object 都在
    // 契约范围内，且 object 先置空以便失败时调用方拿到确定的空指针。
    unsafe {
        *object = core::ptr::null_mut();
        if *rclsid != reg::CLSID_SILK_TIP {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
    }
    let factory: IClassFactory = Factory.into();
    // SAFETY: riid/object 同上；query 只写 *object。
    unsafe { factory.query(riid, object) }
}

/// 无存活实例时允许宿主卸载本 DLL。
#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if tip::instance_count() == 0 {
        HRESULT(0)
    } else {
        HRESULT(1)
    }
}

/// rundll32 入口，签名固定为 `(HWND, HINSTANCE, LPSTR cmd, int)`。
///
/// # Safety
/// `cmd` 由 rundll32 传入 NUL 结尾的 ANSI 串，可能为空。
#[no_mangle]
pub unsafe extern "system" fn SilkRegister(
    _hwnd: HWND,
    _instance: HINSTANCE,
    cmd: *const u8,
    _show: i32,
) {
    // SAFETY: cmd 按 rundll32 契约是 NUL 结尾的 ANSI 串或空指针，cmd_arg 自己判空。
    let scope = unsafe { reg::cmd_arg(cmd) };
    reg::run_register(scope.as_deref());
}

/// 注销入口，逆序清理三类注册。
///
/// # Safety
/// 同 [`SilkRegister`]。
#[no_mangle]
pub unsafe extern "system" fn SilkUnregister(
    _hwnd: HWND,
    _instance: HINSTANCE,
    cmd: *const u8,
    _show: i32,
) {
    // SAFETY: 同 SilkRegister。
    let scope = unsafe { reg::cmd_arg(cmd) };
    reg::run_unregister(scope.as_deref());
}

/// 补登记：把已有 profile 重新启用进当前用户键盘列表。
#[no_mangle]
pub extern "system" fn SilkRegisterSelf() -> HRESULT {
    if reg::register_self() {
        HRESULT(0)
    } else {
        HRESULT(0x80004005u32 as i32)
    }
}

/// DllMain 只保存模块句柄：这里不允许建 COM 对象、加载渲染栈或起线程。
#[no_mangle]
extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut core::ffi::c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        set_module(module);
    } else if reason == DLL_PROCESS_DETACH {
        MODULE.store(0, Ordering::Release);
    }
    BOOL::from(true)
}

