//! composition 的创建/更新/提交与插入点度量。
//!
//! 文本修改必须发生在 TSF 的同步 edit session 内（文档锁），业务计算已在 host
//! 完成，这里只做 Range 操作。任何失败都降级为「隐藏候选」，不向 TSF 冒泡。

use std::sync::{Arc, Mutex};

use windows::core::{implement, Interface, Result};
use windows::core::BOOL as WinBool;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::TextServices::{
    INSERT_TEXT_AT_SELECTION_FLAGS, ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition, ITfContextView,
    ITfEditSession, ITfEditSession_Impl, ITfInsertAtSelection, ITfRange, TF_ANCHOR_START,
    GUID_PROP_ATTRIBUTE, TF_ES_READWRITE, TF_ES_SYNC, TF_IAS_NO_DEFAULT_COMPOSITION,
    TF_IAS_QUERYONLY,
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
        let range = unsafe { composition.GetRange() }?;
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
                if let Ok(range) = unsafe { composition.GetRange() } {
                    let _ = unsafe { range.SetText(ec, 0, &util::utf16(text)) };
                }
                let _ = unsafe { composition.EndComposition(ec) };
            }
            None if !text.is_empty() => {
                let insert: ITfInsertAtSelection = self.ctx.cast()?;
                let _ = unsafe { insert.InsertTextAtSelection(ec, INSERT_TEXT_AT_SELECTION_FLAGS(0), &util::utf16(text)) };
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
            if let Ok(range) = unsafe { composition.GetRange() } {
                let _ = unsafe { range.SetText(ec, 0, &[]) };
            }
            let _ = unsafe { composition.EndComposition(ec) };
        }
    }

    fn ensure(&self, ec: u32) -> Result<ITfComposition> {
        if let Some(composition) = self.session.lock().composition.clone() {
            return Ok(composition);
        }
        let insert: ITfInsertAtSelection = self.ctx.cast()?;
        // 取插入点上的空 range：不插入文本，也不要 TSF 的默认 composition。
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
        unsafe { comps.StartComposition(ec, &range, &self.sink) }
    }
}

/// 插入点屏幕坐标：GetTextExt 优先，失败退回 GUI 线程光标，再退回窗口矩形。
fn caret_rect(ctx: &ITfContext, ec: u32) -> Option<Rect> {
    if let Ok(view) = unsafe { ctx.GetActiveView() } {
        if let Some(rect) = caret_from_view(ctx, &view, ec) {
            return Some(rect);
        }
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
    unsafe { ctx.GetSelection(ec, 0, &mut selections, &mut fetched) }.ok()?;
    let range = (*selections[0].range).clone()?;
    unsafe { range.Collapse(ec, TF_ANCHOR_START) }.ok()?;
    let mut rect = RECT::default();
    let mut clipped = WinBool::default();
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

fn caret_from_window(fallback: HWND) -> Option<Rect> {
    let mut hwnd = fallback;
    if hwnd.0.is_null() {
        hwnd = unsafe { GetForegroundWindow() };
    }
    if hwnd.0.is_null() {
        return None;
    }
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if tid != 0 {
        let mut info = GUITHREADINFO {
            cbSize: core::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
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
    let mgr: windows::Win32::UI::TextServices::ITfCategoryMgr = unsafe {
        windows::Win32::System::Com::CoCreateInstance(
            &windows::Win32::UI::TextServices::CLSID_TF_CategoryMgr,
            None,
            windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
        )
    }
    .ok()?;
    let atom = unsafe { mgr.RegisterGUID(&crate::ui::GUID_SILK_DISPLAY_ATTRIBUTE) }.ok()?;
    ATTR_ATOM.with(|cell| cell.set(Some(atom)));
    Some(atom)
}

/// 给 range 打 GUID_PROP_ATTRIBUTE：显示属性提供者据此画下划线。
fn apply_underline(ctx: &ITfContext, ec: u32, range: &ITfRange) {
    let Some(atom) = display_attribute_atom() else {
        return;
    };
    let Ok(property) = (unsafe { ctx.GetProperty(&GUID_PROP_ATTRIBUTE) }) else {
        return;
    };
    let mut value = windows::Win32::System::Variant::VARIANT::default();
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
