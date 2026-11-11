//! 窗口与 UI 线程：建 owned popup、定位、主题、渲染调度。
//!
//! 导出函数只把视图快照拷进命令队列并唤醒自建 UI 线程；所有 Win32/D2D
//! 调用都发生在该线程，调用方（host 的多线程或 shim）不阻塞、不接触图形
//! 对象。窗口状态归 UI 线程独占，跨线程只有一条 `mpsc` 通道。

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{LazyLock, Mutex};
use std::thread::ThreadId;

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUNDSMALL,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    PAINTSTRUCT,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::NotifyWinEvent;
use windows::Win32::UI::WindowsAndMessaging::OBJID_CLIENT;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW, IsWindow,
    PostThreadMessageW, RegisterClassW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, EVENT_OBJECT_IME_CHANGE, EVENT_OBJECT_IME_HIDE, EVENT_OBJECT_IME_SHOW,
    GWL_EXSTYLE, GWLP_HWNDPARENT, HWND_TOPMOST, MA_NOACTIVATE, MSG, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNA, WM_APP, WM_DPICHANGED, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_PAINT, WM_SETTINGCHANGE,
    WM_THEMECHANGED, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::layout::{self, IRect, METRICS};
use crate::model::Snapshot;
use crate::render::{Factory, Renderer};
use crate::scene::Scene;
use crate::text::TextEngine;
use crate::theme::Theme;

/// 插入点与候选窗之间的垂直间距（DIP）。
const ANCHOR_GAP_DIP: f32 = 4.0;
/// 自定义消息：有新命令入队。
const WM_APP_COMMAND: u32 = WM_APP + 1;

#[derive(Debug)]
pub enum Command {
    Show {
        snapshot: Snapshot,
        anchor: IRect,
        owner: isize,
    },
    Move {
        anchor: IRect,
    },
    Hide,
}

struct Ui {
    hwnd: HWND,
    factory: Factory,
    engine: TextEngine,
    renderer: Option<Renderer>,
    theme: Theme,
    scene: Option<Scene>,
    anchor: IRect,
    owner: isize,
    dpi: u32,
    visible: bool,
    /// 上一帧的像素尺寸：尺寸不变时 move 不必重画（内容相同，只挪位置）。
    last_size: (i32, i32),
}

impl Ui {
    fn new() -> Result<Self> {
        // SAFETY: 只查询本模块句柄，无所有权转移。
        let hinstance = unsafe { GetModuleHandleW(None)? };
        let class_name: Vec<u16> = "SilkUiCandidateWindow\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: 类结构完整、字符串存活；重复注册返回 0（同进程仅一份），无害。
        unsafe {
            RegisterClassW(&wc);
        }
        // SAFETY: 类已注册；WS_POPUP 无 WS_VISIBLE，创建后不可见。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(class_name.as_ptr()),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?
        };
        Ok(Self {
            hwnd,
            factory: Factory::new()?,
            engine: TextEngine::new()?,
            renderer: None,
            theme: Theme::detect(),
            scene: None,
            anchor: IRect::new(0, 0, 0, 0),
            owner: 0,
            dpi: 96,
            visible: false,
            last_size: (0, 0),
        })
    }

    /// 圆角/深色/边框全交 DWM；主题切换时重下发，不重建窗口（06）。
    fn apply_dwm(&self) {
        let corner = DWMWCP_ROUNDSMALL;
        let dark = self.theme.dark as u32;
        let border = self.theme.border.to_colorref();
        // SAFETY: 窗口有效；属性值都是 POD，大小与 DWM 期望一致。
        unsafe {
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const _ as *const core::ffi::c_void,
                core::mem::size_of_val(&corner) as u32,
            );
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const core::ffi::c_void,
                core::mem::size_of_val(&dark) as u32,
            );
            let _ = DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_BORDER_COLOR,
                &border as *const _ as *const core::ffi::c_void,
                core::mem::size_of_val(&border) as u32,
            );
        }
    }

    fn set_owner(&mut self, owner: isize) {
        // 宿主可能传 0 或失效句柄（插入点所在窗口已销毁）；非窗口一律按"无所有者"处理。
        // SAFETY: 只读查询，句柄无效返回 FALSE。
        let owner = if owner != 0 && unsafe { IsWindow(Some(HWND(owner as *mut _))) }.as_bool() {
            owner
        } else {
            0
        };
        if owner == self.owner {
            return;
        }
        self.owner = owner;
        // SAFETY: 只改窗口的 owner 字段；0 即清除所有者。
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_HWNDPARENT, owner);
        }
    }

    /// 首次显示时选后端：主路（flip 交换链）不可用则退分层窗口 + DC 目标。
    fn ensure_backend(&mut self) {
        if self.renderer.is_some() {
            return;
        }
        // 诊断开关：`SILKUI_GPU=0` 强制走兜底路，供联调与 O-11 帧成本测量。
        let force_dcr = std::env::var_os("SILKUI_GPU").is_some_and(|v| v == "0");
        if force_dcr {
            self.renderer = match Renderer::new_dcr(&self.factory, self.dpi) {
                Ok(r) => {
                    self.set_layered(true);
                    Some(r)
                }
                Err(_) => None,
            };
            return;
        }
        self.renderer = Some(match Renderer::new_gpu(self.hwnd, &self.factory, self.dpi) {
            Ok(r) => r,
            Err(_) => match Renderer::new_dcr(&self.factory, self.dpi) {
                Ok(r) => {
                    self.set_layered(true);
                    r
                }
                Err(_) => return,
            },
        });
        if self.is_layered() {
            self.set_layered(true);
        }
    }

    fn set_layered(&self, on: bool) {
        // SAFETY: 仅调整扩展样式位；下一次 SetWindowPos 生效。
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let bit = WS_EX_LAYERED.0 as isize;
            let next = if on { style | bit } else { style & !bit };
            if next != style {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, next);
                // 运行时切换 WS_EX_LAYERED 必须让系统重建非客户区，否则样式不生效。
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
        }
    }

    fn is_layered(&self) -> bool {
        self.renderer.as_ref().is_some_and(Renderer::needs_layered)
    }

    fn show(&mut self, snapshot: Snapshot, anchor: IRect, owner: isize) {
        let started = std::time::Instant::now();
        if snapshot.is_empty() {
            self.hide();
            return;
        }
        self.set_owner(owner);
        self.anchor = anchor;
        self.dpi = dpi_for_rect(anchor);
        self.ensure_backend();
        let scene = match Scene::build(&self.engine, &snapshot, &METRICS, self.is_layered()) {
            Ok(s) => s,
            // 排版失败（字体缺失等）：降级隐藏，绝不阻塞输入。
            Err(_) => {
                self.hide();
                return;
            }
        };
        self.scene = Some(scene);
        let ok = self.commit_inner(true);
        // 帧预算（06）：候选更新到可见 ≤ 16 ms。诊断开关只打印，不参与决策。
        if std::env::var_os("SILKUI_TRACE").is_some() {
            eprintln!("silkui: show frame {:?}", started.elapsed());
        }
        if !ok {
            self.hide();
        }
    }

    fn move_to(&mut self, anchor: IRect) {
        self.anchor = anchor;
        if !self.visible || self.scene.is_none() {
            return;
        }
        let dpi = dpi_for_rect(anchor);
        // 跨显示器才需要按新 DPI 重排版；同 DPI 下内容不变，只挪位置。
        let repaint = dpi != self.dpi;
        self.dpi = dpi;
        if !self.commit_inner(repaint) {
            self.hide();
        }
    }

    /// 尺寸 + 位置一次 `SetWindowPos` 提交（原子移动，不闪烁），随后按需画一帧。
    fn commit(&mut self) -> bool {
        self.commit_inner(true)
    }

    fn commit_inner(&mut self, repaint: bool) -> bool {
        let Some(scene) = &self.scene else {
            return false;
        };
        let size = scene.extent_px(self.dpi);
        let repaint = repaint || size != self.last_size;
        let gap = layout::dip_to_px(ANCHOR_GAP_DIP, self.dpi);
        let placement = layout::place(self.anchor, size, gap, work_area(self.anchor));
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        if renderer
            .resize(self.hwnd, &self.factory, size, self.dpi)
            .is_err()
        {
            return false;
        }
        // SAFETY: 窗口与渲染器均有效；先定位定尺寸（不改变可见性），画完再显示。
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                placement.x,
                placement.y,
                size.0,
                size.1,
                SWP_NOACTIVATE,
            );
        }
        self.apply_dwm();
        self.last_size = size;
        if repaint && !self.paint() {
            return false;
        }
        if !self.visible {
            // SAFETY: 分层窗口已由 UpdateLayeredWindow 提交内容；主路在此显示。
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                NotifyWinEvent(EVENT_OBJECT_IME_SHOW, self.hwnd, OBJID_CLIENT.0, 0);
            }
            self.visible = true;
        } else {
            // SAFETY: 内容变化参与系统 light dismiss。
            unsafe { NotifyWinEvent(EVENT_OBJECT_IME_CHANGE, self.hwnd, OBJID_CLIENT.0, 0) };
        }
        true
    }

    /// 画一帧。失败按设备丢失处理：重建渲染目标重画该帧（06 故障恢复），
    /// 仍失败才降级隐藏。
    fn paint(&mut self) -> bool {
        if self.paint_once() {
            return true;
        }
        self.renderer = None;
        self.ensure_backend();
        self.paint_once()
    }

    fn paint_once(&mut self) -> bool {
        let (Some(scene), Some(renderer)) = (&self.scene, &mut self.renderer) else {
            return false;
        };
        let theme = self.theme;
        let size = self.last_size;
        if renderer
            .resize(self.hwnd, &self.factory, size, self.dpi)
            .is_err()
        {
            return false;
        }
        renderer
            .present(self.hwnd, scene, &theme, self.engine.rendering_params())
            .is_ok()
    }

    fn hide(&mut self) {
        if self.visible {
            // SAFETY: 窗口有效；Hide 事件让系统释放 light dismiss 状态。
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                NotifyWinEvent(EVENT_OBJECT_IME_HIDE, self.hwnd, OBJID_CLIENT.0, 0);
            }
            self.visible = false;
        }
        self.scene = None;
    }

    fn on_dpi_changed(&mut self, dpi: u32) {
        if dpi == 0 || dpi == self.dpi {
            return;
        }
        self.dpi = dpi;
        if self.visible && self.scene.is_some() && !self.commit() {
            self.hide();
        }
    }

    fn on_theme_changed(&mut self) {
        self.theme = Theme::detect();
        self.apply_dwm();
        if self.visible && self.scene.is_some() && !self.paint() {
            self.hide();
        }
    }
}

fn work_area(anchor: IRect) -> IRect {
    let rect = RECT {
        left: anchor.left,
        top: anchor.top,
        right: anchor.right,
        bottom: anchor.bottom,
    };
    let mut info = MONITORINFO {
        cbSize: core::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: 结构与输出指针有效。
    let ok = unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        !monitor.0.is_null() && GetMonitorInfoW(monitor, &mut info).as_bool()
    };
    if !ok {
        return IRect::new(0, 0, 0, 0);
    }
    IRect::new(
        info.rcWork.left,
        info.rcWork.top,
        info.rcWork.right,
        info.rcWork.bottom,
    )
}

fn dpi_for_rect(anchor: IRect) -> u32 {
    let rect = RECT {
        left: anchor.left,
        top: anchor.top,
        right: anchor.right,
        bottom: anchor.bottom,
    };
    // SAFETY: 输出指针有效；失败按 96 处理（库不自行声明 DPI 感知等级）。
    unsafe {
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (0u32, 0u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_ok() && x > 0 {
            x
        } else {
            96
        }
    }
}

thread_local! {
    /// UI 线程独占的窗口状态。`SetWindowPos` 可能在内部同步派发
    /// `WM_DPICHANGED`（重入），所以用 `try_borrow_mut` 进入消息处理。
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 06：永不抢焦点——点击只选候选，不激活窗口。
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        // 背景由渲染器整块覆盖（不透明主路 / alpha DIB 兜底路）。
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            // 必须 BeginPaint/EndPaint 校验更新区：否则 WM_PAINT 会被反复投递，
            // 消息循环空转（候选窗按需重画，不靠重绘风暴）。
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: ps 可写；返回的 HDC 只在该 PAINTSTRUCT 生命周期内使用。
            unsafe {
                let _ = BeginPaint(hwnd, &mut ps);
            }
            with_ui(|ui| {
                ui.paint();
            });
            // SAFETY: 与 BeginPaint 配对。
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xFFFF) as u32;
            with_ui(|ui| ui.on_dpi_changed(dpi));
            return LRESULT(0);
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            with_ui(Ui::on_theme_changed);
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: 默认处理未处理的消息。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn with_ui(f: impl FnOnce(&mut Ui)) {
    let _ = UI.try_with(|slot| {
        // 消息处理可能由 SetWindowPos/ShowWindow 内部重入，占用中的话跳过这一帧。
        if let Ok(mut slot) = slot.try_borrow_mut() {
            if let Some(ui) = slot.as_mut() {
                f(ui);
            }
        }
    });
}

struct UiHandle {
    tx: Sender<Command>,
    /// Win32 线程 id，用于 `PostThreadMessageW` 唤醒。
    tid: u32,
    /// Rust 线程 id，用于识别"命令来自 UI 线程本身"。
    thread: ThreadId,
}

impl UiHandle {
    fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_ok() {
            // SAFETY: 目标线程有自己的消息循环；线程已退出时静默失败。
            unsafe {
                let _ = PostThreadMessageW(self.tid, WM_APP_COMMAND, WPARAM(0), LPARAM(0));
            }
        }
    }
}

static CHANNEL: LazyLock<Mutex<Channel>> = LazyLock::new(|| Mutex::new(Channel::default()));

#[derive(Default)]
struct Channel {
    handle: Option<UiHandle>,
    /// 创建失败后不再重试：候选窗是增强项，失败即无窗运行。
    failed: bool,
    /// UI 线程自身 id：命令来自该线程时直接丢弃（排队会重入渲染）。
    ui_thread: Option<ThreadId>,
}

/// 把命令排队给 UI 线程；首次调用时创建线程。任何失败都静默降级。
pub fn dispatch(cmd: Command) {
    let mut ch = match CHANNEL.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if ch.ui_thread == Some(std::thread::current().id()) {
        return;
    }
    if ch.handle.is_none() {
        if ch.failed {
            return;
        }
        match spawn_ui_thread() {
            Some(h) => {
                ch.ui_thread = Some(h.thread);
                ch.handle = Some(h);
            }
            None => {
                ch.failed = true;
                return;
            }
        }
    }
    if let Some(handle) = &ch.handle {
        handle.send(cmd);
    }
}

fn spawn_ui_thread() -> Option<UiHandle> {
    let (tx, rx) = channel();
    let (ready_tx, ready_rx) = channel::<Option<(u32, ThreadId)>>();
    std::thread::Builder::new()
        .name("silkui-ui".into())
        .spawn(move || ui_main(rx, ready_tx))
        .ok()?;
    match ready_rx.recv() {
        Ok(Some((tid, thread))) => Some(UiHandle { tx, tid, thread }),
        _ => None,
    }
}

fn ui_main(rx: Receiver<Command>, ready: Sender<Option<(u32, ThreadId)>>) {
    // SAFETY: 初始化单线程套间；已初始化时同样返回 Ok。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    match Ui::new() {
        Ok(ui) => {
            UI.with(|slot| *slot.borrow_mut() = Some(ui));
            // SAFETY: 取当前线程 id 供 PostThreadMessageW 唤醒本线程消息循环。
            let tid = unsafe { GetCurrentThreadId() };
            UI_TID.store(tid, Ordering::Relaxed);
            let _ = ready.send(Some((tid, std::thread::current().id())));
            run_loop(&rx);
            with_ui(Ui::hide);
            UI.with(|slot| slot.borrow_mut().take());
        }
        Err(_) => {
            let _ = ready.send(None);
        }
    }
    // SAFETY: 与上面的 CoInitializeEx 配对。
    unsafe { CoUninitialize() };
}

fn run_loop(rx: &Receiver<Command>) {
    loop {
        let mut msg = MSG::default();
        // SAFETY: msg 可写；None 表示收取本线程全部消息（含 PostThreadMessage）。
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            return;
        }
        if msg.message == WM_APP_COMMAND {
            // 批量应用队列里的命令：连续 update 合并为一帧（06 合并渲染指令）。
            while let Ok(cmd) = rx.try_recv() {
                apply(cmd);
            }
            continue;
        }
        // SAFETY: msg 来自 GetMessageW。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn apply(cmd: Command) {
    with_ui(|ui| match cmd {
        Command::Show {
            snapshot,
            anchor,
            owner,
        } => ui.show(snapshot, anchor, owner),
        Command::Move { anchor } => ui.move_to(anchor),
        Command::Hide => ui.hide(),
    });
}

static UI_TID: AtomicU32 = AtomicU32::new(0);
