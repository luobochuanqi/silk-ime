//! composition 的创建/更新/提交与插入点度量。
//!
//! 文本修改必须发生在 TSF 的同步 edit session 内（文档锁），业务计算已在 host
//! 完成，这里只做 Range 操作。任何失败都降级为「隐藏候选」，不向 TSF 冒泡。

use std::sync::{Arc, Mutex};

use windows::core::BOOL as WinBool;
use windows::core::{implement, Interface, Result};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::TextServices::{
    INSERT_TEXT_AT_SELECTION_FLAGS, ITfComposition, ITfCompositionSink, ITfContext,
    ITfContextComposition, ITfContextView, ITfEditSession, ITfEditSession_Impl,
    ITfInsertAtSelection, ITfRange, TF_ANCHOR_START, TF_ES_READWRITE, TF_ES_SYNC,
    TF_IAS_NO_DEFAULT_COMPOSITION, TF_IAS_QUERYONLY, GUID_PROP_ATTRIBUTE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowRect, GetWindowThreadProcessId, GUITHREADINFO,
};

use crate::msg::{self, Rect};
use crate::session::{EditIntent, Session};
use crate::util;

/// edit session 里的动作。
enum Action {
    Update(String),
    Commit(String),
}

/// 一次同步 edit session：更新 composition 文本，顺带把插入点记下来。
#[implement(ITfEditSession)]
struct Edit {
    session: Arc<Session>,
    ctx: ITfContext,
    sink: ITfCompositionSink,
    action: Action,
    caret: Arc<Mutex<Option<Rect>>>,
}

impl ITfEditSession_Impl for Edit_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        *self.caret.lock().unwrap_or_else(|e| e.into_inner()) = caret_rect(&self.ctx, ec);
        match &self.action {
            Action::Update(text) => self.update(ec, text),
            Action::Commit(text) => self.commit(ec, text),
        }
    }
}

impl Edit {
    fn update(&self, ec: u32, text: &str) -> Result<()> {
        if text.is_empty() {
            self.end(ec);
            return Ok(());
        }
        let composition = self.ensure(ec)?;
        // SAFETY: composition 是本会话持有的有效接口引用；GetRange 只增加引用计数。
        let range = unsafe { composition.GetRange() }?;
        // SAFETY: range 与 ctx 都有效；ec 是 TSF 交给本次 edit session 的当前编辑
        // cookie；utf16 缓冲在调用期间存活，TSF 只读它。
        unsafe { range.SetText(ec, 0, &util::utf16(text)) }?;
        apply_underline(&self.ctx, ec, &range);
        let mut state = self.session.lock();
        state.composition = Some(composition);
        state.preedit = text.to_owned();
        Ok(())
    }

    fn commit(&self, ec: u32, text: &str) -> Result<()> {
        let composition = self.session.lock().composition.take();
        match composition {
            Some(composition) => {
                // SAFETY: composition 有效；range 由它派生，同样有效。
                if let Ok(range) = unsafe { composition.GetRange() } {
                    // SAFETY: 同上；失败不致命，仍要结束 composition。
                    let _ = unsafe { range.SetText(ec, 0, &util::utf16(text)) };
                }
                // SAFETY: composition 有效且未被别处结束过；EndComposition 只做文档
                // 收尾（失败时文档保持原状，不影响后续按键）。
                let _ = unsafe { composition.EndComposition(ec) };
            }
            None if !text.is_empty() => {
                let insert: ITfInsertAtSelection = self.ctx.cast()?;
                // SAFETY: insert 属于当前 context；ec 有效；utf16 缓冲存活到调用结束。
                let _ = unsafe {
                    insert.InsertTextAtSelection(
                        ec,
                        INSERT_TEXT_AT_SELECTION_FLAGS(0),
                        &util::utf16(text),
                    )
                };
            }
            None => {}
        }
        let mut state = self.session.lock();
        state.preedit.clear();
        state.candidates.clear();
        state.page_index = 0;
        state.selected = 0;
        Ok(())
    }

    fn end(&self, ec: u32) {
        let composition = self.session.lock().composition.take();
        if let Some(composition) = composition {
            // SAFETY: composition 有效；先清空文本再结束，避免空 composition 残留。
            if let Ok(range) = unsafe { composition.GetRange() } {
                // SAFETY: 同 update：range/ec 有效，空切片是合法文本。
                let _ = unsafe { range.SetText(ec, 0, &[]) };
            }
            // SAFETY: 同上，EndComposition 结束本会话自己开启的 composition。
            let _ = unsafe { composition.EndComposition(ec) };
        }
    }

    fn ensure(&self, ec: u32) -> Result<ITfComposition> {
        if let Some(composition) = self.session.lock().composition.clone() {
            return Ok(composition);
        }
        let insert: ITfInsertAtSelection = self.ctx.cast()?;
        // 取插入点上的空 range：不插入文本，也不要 TSF 的默认 composition。
        // SAFETY: insert 属于当前 context；ec 有效；空切片表示不写入字符。
        let range = unsafe {
            insert.InsertTextAtSelection(
                ec,
                INSERT_TEXT_AT_SELECTION_FLAGS(
                    TF_IAS_QUERYONLY.0 | TF_IAS_NO_DEFAULT_COMPOSITION.0,
                ),
                &[],
            )
        }?;
        let comps: ITfContextComposition = self.ctx.cast()?;
        // SAFETY: comps 属于当前 context；range 刚由同一 context 取得；sink 是本站
        // 对象的接口引用，TSF 持有它到 composition 结束。
        unsafe { comps.StartComposition(ec, &range, &self.sink) }
    }
}

/// 插入点屏幕坐标：GetTextExt 优先，失败退回 GUI 线程光标，再退回窗口矩形。
fn caret_rect(ctx: &ITfContext, ec: u32) -> Option<Rect> {
    // SAFETY: ctx 有效；GetActiveView 只增加引用计数，失败走降级路径。
    if let Ok(view) = unsafe { ctx.GetActiveView() } {
        if let Some(rect) = caret_from_view(ctx, &view, ec) {
            return Some(rect);
        }
        // SAFETY: view 有效；无窗口的 host（如 UWP 岛）返回错误或空句柄。
        if let Ok(hwnd) = unsafe { view.GetWnd() } {
            if let Some(rect) = caret_from_window(hwnd) {
                return Some(rect);
            }
        }
    }
    caret_from_window(HWND::default())
}

fn caret_from_view(ctx: &ITfContext, view: &ITfContextView, ec: u32) -> Option<Rect> {
    let mut selections = [windows::Win32::UI::TextServices::TF_SELECTION::default()];
    let mut fetched = 0u32;
    // SAFETY: selections 是长度 1 的独占数组、fetched 是本地计数；GetSelection 最多
    // 写 1 项，返回的 range 引用归我们所有，下面用 ManuallyDrop::take 接走并释放。
    unsafe { ctx.GetSelection(ec, 0, &mut selections, &mut fetched) }.ok()?;
    // 文档没有选区（TSF 可以合法地什么都不给）：直接走降级路径，绝不解引用空槽。
    if fetched == 0 {
        return None;
    }
    // SAFETY: fetched == 1 保证 selections[0].range 已由 TSF 填好；take 取走唯一
    // 所有权，元素随后不再被读取（TF_SELECTION 的 ManuallyDrop 因此不会二次释放）。
    let range = unsafe { core::mem::ManuallyDrop::take(&mut selections[0].range) }?;
    // SAFETY: range 有效；Collapse 只调整 range 边界。
    unsafe { range.Collapse(ec, TF_ANCHOR_START) }.ok()?;
    let mut rect = RECT::default();
    let mut clipped = WinBool::default();
    // SAFETY: view/range 有效；rect 与 clipped 是本地可写量，GetTextExt 只写它们。
    unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) }.ok()?;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }
    Some(Rect {
        x: rect.left,
        y: rect.top,
        w: rect.right - rect.left,
        h: rect.bottom - rect.top,
    })
}

/// 06:41 规定插入点坐标的唯一来源是 GetTextExt；这里的窗口回退只在拿不到 view 时
/// 兜底，避免候选窗退回到屏幕角。
fn caret_from_window(fallback: HWND) -> Option<Rect> {
    let mut hwnd = fallback;
    if hwnd.0.is_null() {
        // SAFETY: 无参数、只读查询，返回可能为空的前台窗口句柄。
        hwnd = unsafe { GetForegroundWindow() };
    }
    if hwnd.0.is_null() {
        return None;
    }
    // SAFETY: hwnd 非空且由系统提供；第二个参数为 None 表示不接收 pid。
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if tid != 0 {
        let mut info = GUITHREADINFO {
            cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: tid 来自上面的查询；info 是本地结构，cbSize 已按类型填好。
        if unsafe { GetGUIThreadInfo(tid, &mut info) }.is_ok() && info.rcCaret.right != 0 {
            let caret_hwnd = if info.hwndCaret.0.is_null() {
                hwnd
            } else {
                info.hwndCaret
            };
            let mut point = POINT {
                x: info.rcCaret.left,
                y: info.rcCaret.top,
            };
            // SAFETY: caret_hwnd 是刚由 GetGUIThreadInfo 填写的窗口句柄；point 是本地
            // 可写量，函数只做坐标系转换。
            if unsafe { ClientToScreen(caret_hwnd, &mut point) }.as_bool() {
                return Some(Rect {
                    x: point.x,
                    y: point.y,
                    w: 1,
                    h: (info.rcCaret.bottom - info.rcCaret.top).max(1),
                });
            }
        }
    }
    let mut rect = RECT::default();
    // SAFETY: hwnd 非空；rect 是本地可写量。
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
        return Some(Rect {
            x: rect.left,
            y: rect.top + (rect.bottom - rect.top) / 2,
            w: 1,
            h: ((rect.bottom - rect.top) / 2).max(1),
        });
    }
    None
}

thread_local! {
    /// TfGuidAtom 进程内缓存：RegisterGUID 走 msctf，不该每键重复。
    static ATTR_ATOM: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

fn display_attribute_atom() -> Option<u32> {
    if let Some(atom) = ATTR_ATOM.with(std::cell::Cell::get) {
        return Some(atom);
    }
    // SAFETY: CoCreateInstance 只返回新接口指针；本线程已由 TSF 初始化 COM。
    let mgr: windows::Win32::UI::TextServices::ITfCategoryMgr = unsafe {
        windows::Win32::System::Com::CoCreateInstance(
            &windows::Win32::UI::TextServices::CLSID_TF_CategoryMgr,
            None,
            windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
        )
    }
    .ok()?;
    // SAFETY: mgr 有效；GUID 是静态常量，RegisterGUID 只读它。
    let atom = unsafe { mgr.RegisterGUID(&crate::ui::GUID_SILK_DISPLAY_ATTRIBUTE) }.ok()?;
    ATTR_ATOM.with(|cell| cell.set(Some(atom)));
    Some(atom)
}

/// 给 range 打 GUID_PROP_ATTRIBUTE：显示属性提供者据此画下划线。
fn apply_underline(ctx: &ITfContext, ec: u32, range: &ITfRange) {
    let Some(atom) = display_attribute_atom() else {
        return;
    };
    // SAFETY: ctx 有效；GetProperty 返回 doc 属性对象或错误，失败即放弃下划线。
    let Ok(property) = (unsafe { ctx.GetProperty(&GUID_PROP_ATTRIBUTE) }) else {
        return;
    };
    let mut value = windows::Win32::System::Variant::VARIANT::default();
    // SAFETY: VARIANT 是本栈上的独占值；Anonymous 联合按当前 vt=VT_I4 访问 lVal，
    // 指针由 union 自身派生且立即使用；property/range/ec 均有效。
    unsafe {
        let inner = &mut *value.Anonymous.Anonymous;
        inner.vt = windows::Win32::System::Variant::VT_I4;
        inner.Anonymous.lVal = atom as i32;
        let _ = property.SetValue(ec, range, &value);
    }
}

/// 应用一次 host 意图（同步 edit session + 候选登记 + 插入点回填）。
pub fn apply(session: &Arc<Session>, intent: &EditIntent) -> Result<()> {
    let (ctx, tid, sink) = {
        let state = session.lock();
        match (&state.context, state.tid, state.composition_sink.clone()) {
            (Some(ctx), tid, Some(sink)) if tid != 0 => (ctx.clone(), tid, sink),
            // 没有 sink 说明焦点会话未就绪：静默放弃，按键将于下一键重试。
            _ => return Ok(()),
        }
    };
    let action = match intent {
        EditIntent::Update(update) => Action::Update(update.preedit.clone()),
        EditIntent::Commit(text) => Action::Commit(text.clone()),
    };
    let edit = Edit {
        session: Arc::clone(session),
        ctx: ctx.clone(),
        sink,
        action,
        caret: Arc::new(Mutex::new(None)),
    };
    let caret_slot = Arc::clone(&edit.caret);
    let session_obj: ITfEditSession = edit.into();
    // SAFETY: ctx 有效；session_obj 是本站对象，TSF 在 RequestEditSession 返回前
    // 已按同步语义完成回调（引用计数由 TSF 维护）；tid 来自 ActivateEx。
    let hr = unsafe { ctx.RequestEditSession(tid, &session_obj, TF_ES_SYNC | TF_ES_READWRITE) };
    if hr.is_err() || hr.is_ok_and(|r| r.is_err()) {
        // 文档锁拿不到（只读文档、宿主拒绝）：隐藏候选，不上屏也不吞键。
        let mut state = session.lock();
        state.candidates.clear();
        state.ui_id = None;
        state.shown = false;
        return Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_FAIL,
        ));
    }
    if let Some(rect) = *caret_slot.lock().unwrap_or_else(|e| e.into_inner()) {
        let sid = session.lock().sid.clone();
        if !sid.is_empty() {
            session.post(&msg::caret(0, &sid, rect));
        }
    }
    crate::ui::sync(session, &ctx);
    Ok(())
}
