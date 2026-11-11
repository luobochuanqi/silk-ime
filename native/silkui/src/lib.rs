//! Silk IME 候选窗渲染库（C ABI，产物 silkui.dll）。
//!
//! 接口契约与渲染设计见 docs/design/foundation/06-ui.md。导出面只有
//! `silkui_show` / `silkui_move` / `silkui_hide` 三个函数：任意线程可调，
//! 库内拷贝输入后串行到自建 UI 线程。所有导出函数都不 panic、不抛异常
//! （`panic = abort` 的替代是无法跨 FFI 捕获，故实现内不使用 `unwrap`
//! 于外部输入路径）。

mod layout;
mod model;
mod render;
mod scene;
mod text;
mod theme;
mod window;

use core::ffi::{c_char, c_void};

use crate::layout::IRect;
use crate::model::Snapshot;
use crate::window::Command;

/// 单条候选。字符串为 UTF-8 NUL 结尾，仅在调用期间有效。
#[repr(C)]
pub struct SilkCandidate {
    pub text: *const c_char,
    pub comment: *const c_char,
}

/// 一帧候选视图。指针仅在调用期间有效，库内拷贝。
#[repr(C)]
pub struct SilkView {
    pub preedit: *const c_char,
    pub candidates: *const SilkCandidate,
    pub count: i32,
    pub page_index: i32,
}

/// 屏幕物理像素矩形。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SilkRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl SilkRect {
    /// # Safety
    /// `ptr` 要么为空，要么指向一个有效的 `SilkRect`。
    unsafe fn to_irect(ptr: *const SilkRect) -> Option<IRect> {
        if ptr.is_null() {
            return None;
        }
        // SAFETY: 由调用方契约保证可读。
        let r = unsafe { *ptr };
        Some(IRect::from_xywh(r.x, r.y, r.w, r.h))
    }
}

/// 显示或刷新候选窗。
///
/// # Safety
/// `view` 可为空（等价于隐藏）；非空时其内部指针须在调用期间有效。
/// `anchor` 为插入点屏幕矩形；`owner` 为插入点所在窗口（可为 0）。
#[no_mangle]
pub unsafe extern "system" fn silkui_show(
    view: *const SilkView,
    anchor: *const SilkRect,
    owner: *mut c_void,
) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: 契约见函数文档。
        let snapshot = match unsafe { Snapshot::copy_from(view) } {
            Some(s) => s,
            None => {
                window::dispatch(Command::Hide);
                return;
            }
        };
        // 无插入点（坐标过期）就隐藏：06 要求坐标不可信时不显示错误位置。
        let Some(anchor) = (unsafe { SilkRect::to_irect(anchor) }) else {
            window::dispatch(Command::Hide);
            return;
        };
        window::dispatch(Command::Show {
            snapshot,
            anchor,
            owner: owner as isize,
        });
    });
}

/// 只移动已显示的候选窗（一次 `SetWindowPos`，不重排版）。
///
/// # Safety
/// `anchor` 要么为空，要么指向有效的 `SilkRect`。
#[no_mangle]
pub unsafe extern "system" fn silkui_move(anchor: *const SilkRect) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: 契约见函数文档；空指针表示"无有效插入点"，保持原位。
        if let Some(anchor) = unsafe { SilkRect::to_irect(anchor) } {
            window::dispatch(Command::Move { anchor });
        }
    });
}


/// 隐藏候选窗；隐藏是幂等的，无论当前是否可见都可调用。
#[no_mangle]
pub extern "system" fn silkui_hide() {
    let _ = std::panic::catch_unwind(|| window::dispatch(Command::Hide));
}
