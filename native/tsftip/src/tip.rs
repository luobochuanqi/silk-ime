//! TIP 对象：COM 类、ITfTextInputProcessorEx 与四个 sink。
//!
//! 每个文本线程一个实例（Apartment）。所有回调只做三件事：读本地缓存、投递
//! 有界等待的协议请求、把结果落到 composition；业务与 IO 都不在这里。

use std::sync::Arc;

use windows::core::{implement, Interface, Ref, Result, GUID};
use windows::core::BOOL;
use windows::Win32::Foundation::{E_FAIL, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetKeyboardState, MapVirtualKeyW, ToUnicodeEx, HKL, MAPVK_VK_TO_VSC, VK_SHIFT,
};
use windows::core::IUnknownImpl;
use windows::Win32::UI::TextServices::{
    ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDocumentMgr, ITfEditRecord,
    ITfInputProcessorProfileActivationSink, ITfInputProcessorProfileActivationSink_Impl,
    ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource, ITfTextEditSink,
    ITfTextEditSink_Impl, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl,
    TF_TMF_ACTIVATED,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::edit;
use crate::key::{self, Decision, ShiftTap};
use crate::msg;
use crate::reg::GUID_SILK_PROFILE;
use crate::session::Session;

/// 本进程内所有实例的存活计数；DllCanUnloadNow 据此回答。
static INSTANCES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn instance_count() -> usize {
    INSTANCES.load(std::sync::atomic::Ordering::Acquire)
}

/// TIP 的 COM 实现。字段只持有 Arc 与 sink 接口，故可跨 Apartment。
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfTextEditSink,
    ITfCompositionSink,
    ITfInputProcessorProfileActivationSink
)]
pub struct Tip {
    pub session: Arc<Session>,
    /// 本实例的 composition sink 接口；编辑时回传给 TSF。
    pub composition: std::sync::Mutex<Option<ITfCompositionSink>>,
    pub shift: std::sync::Mutex<ShiftTap>,
    pub activation_cookie: std::sync::atomic::AtomicU32,
    pub focus_hwnd: std::sync::atomic::AtomicIsize,
}

impl Tip {
    pub fn new() -> Tip {
        INSTANCES.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Tip {
            session: Session::new(),
            composition: std::sync::Mutex::new(None),
            shift: std::sync::Mutex::new(ShiftTap::default()),
            activation_cookie: std::sync::atomic::AtomicU32::new(0),
            focus_hwnd: std::sync::atomic::AtomicIsize::new(0),
        }
    }

    /// 通知 host 焦点离开，并把未上屏内容原样上屏（03 失焦策略）。
    fn leave(&self) {
        let (sid, context) = {
            let state = self.session.lock();
            (state.sid.clone(), state.context.clone())
        };
        if sid.is_empty() {
            return;
        }
        let reply = self.session.request(&msg::focus_out(0, &sid), true);
        if let Some(context) = context {
            if let crate::client::Reply::Messages(messages) = reply {
                for intent in self.session.absorb(&messages) {
                    let _ = edit::apply(&self.session, &intent);
                }
            }
            crate::ui::end(&self.session, &context);
        }
        self.session.post(&msg::bye(&sid));
        self.session.clear_composition();
        let mut state = self.session.lock();
        state.context = None;
        state.doc_mgr = None;
        state.sid.clear();
    }

    fn enter(&self, ctx: &ITfContext) {
        let hwnd = unsafe { GetForegroundWindow() };
        let app = process_name(hwnd);
        let sid = self.session.next_sid();
        {
            let mut state = self.session.lock();
            state.context = Some(ctx.clone());
            state.doc_mgr = unsafe { ctx.GetDocumentMgr() }.ok();
            state.hwnd = hwnd;
            state.sid = sid.clone();
            state.preedit.clear();
            state.candidates.clear();
            state.composition = None;
            state.ui_id = None;
            state.shown = false;
        }
        let env = msg::focus_in(0, &sid, &app, hwnd.0 as isize as u64);
        if let crate::client::Reply::Messages(messages) = self.session.request(&env, true) {
            for intent in self.session.absorb(&messages) {
                let _ = edit::apply(&self.session, &intent);
            }
        }
    }

    /// 按下入口。返回值写入 *pfEaten。
    fn key_down(&self, ctx: &ITfContext, wparam: WPARAM, lparam: LPARAM) -> bool {
        let vk = (wparam.0 & 0xFFFF) as u16;
        let mods = key::current_mods();

        let decision = self.session.decide(vk, mods);
        match decision {
            Decision::Pass => false,
            Decision::Toggle => self.toggle_mode(),
            Decision::Forward(code) => {
                let rune = if code == key::KEY_PUNCT {
                    char_from_key(vk, lparam)
                } else {
                    None
                };
                if code == key::KEY_ESC {
                    // Esc 走 action 通道：引擎的 cancel 语义与直接转发 Esc 相同，
                    // 但不需要构造字符载荷。
                    let sid = self.session.lock().sid.clone();
                    let eaten = !sid.is_empty()
                        && self.session.handle_action(&msg::action_cancel(0, &sid));
                    crate::ui::sync(&self.session, ctx);
                    return eaten;
                }
                let ts = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
                let outcome = self.session.handle_key(code, rune, true, mods, ts);
                if outcome.eaten {
                    crate::ui::sync(&self.session, ctx);
                }
                outcome.eaten
            }
        }
    }

    /// 显式切换（不在热路径上要求零延迟，但仍用同一 15 ms 预算）。
    fn toggle_mode(&self) -> bool {
        let mode = self.session.mode();
        let sid = self.session.lock().sid.clone();
        if sid.is_empty() {
            return false;
        }
        let env = msg::action_toggle(0, &sid, !mode.chinese, mode.full_punct);
        self.session.handle_action(&env)
    }

    fn select(&self, ctx: &ITfContext, index: u32) -> bool {
        let sid = self.session.lock().sid.clone();
        if sid.is_empty() {
            return false;
        }
        let env = msg::action_select(0, &sid, index);
        let eaten = self.session.handle_action(&env);
        crate::ui::sync(&self.session, ctx);
        eaten
    }

    fn page(&self, ctx: &ITfContext, delta: i32) -> bool {
        let sid = self.session.lock().sid.clone();
        if sid.is_empty() {
            return false;
        }
        let env = msg::action_page(0, &sid, delta);
        let eaten = self.session.handle_action(&env);
        crate::ui::sync(&self.session, ctx);
        eaten
    }

    /// 数字键在组合期被解释为「选第 N 个候选」，不再作为 key 上送。
    ///
    /// 返回 Some(Some(index)) 表示命中当前页的候选；Some(None) 表示越界——
    /// 越界数字必须被吞掉，否则应用会插入一个阿拉伯数字（引擎侧同语义）。
    fn digit_select(&self, vk: u16) -> Option<Option<u32>> {
        let code = key::alpha_digit_code(vk)?;
        if code < key::KEY_0 {
            return None;
        }
        let state = self.session.lock();
        if !state.composing() {
            return None;
        }
        // 数字键 1..9 → 候选 0..8，0 → 候选 9（与微软拼音一致）。
        let digit = (code - key::KEY_0) as u32;
        let index = if digit == 0 { 9 } else { digit - 1 };
        Some((index < state.candidates.len() as u32).then_some(index))
    }
}

impl Drop for Tip {
    fn drop(&mut self) {
        INSTANCES.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn process_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let pid = unsafe {
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
            hwnd,
            Some(&mut pid),
        );
        pid
    };
    if pid == 0 {
        return String::new();
    }
    let Ok(handle) = (unsafe {
        windows::Win32::System::Threading::OpenProcess(
            windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
    }) else {
        return String::new();
    };
    let mut buf = [0u16; 512];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        windows::Win32::System::Threading::QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    if ok.is_err() {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..size as usize]);
    path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned()
}

/// 用当前键盘布局把 VK 译成字符；标点只需要它来填 rune 载荷。
fn char_from_key(vk: u16, lparam: LPARAM) -> Option<char> {
    let mut state = [0u8; 256];
    unsafe { GetKeyboardState(&mut state) }.ok()?;
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let layout: HKL = unsafe { GetKeyboardLayout(0) };
    let mut buf = [0u16; 8];
    let len = unsafe { ToUnicodeEx(vk as u32, scan as u32, &state, &mut buf, 0, Some(layout)) };
    let _ = lparam;
    if len <= 0 {
        return None;
    }
    char::from_u32(buf[0] as u32)
}

impl ITfTextInputProcessor_Impl for Tip_Impl {
    fn Activate(&self, _ptim: Ref<ITfThreadMgr>, _tid: u32) -> Result<()> {
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        self.leave();
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for Tip_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, _flags: u32) -> Result<()> {
        let thread_mgr = ptim.cloned();
        let Some(thread_mgr) = thread_mgr else {
            return Err(windows::core::Error::from_hresult(E_FAIL));
        };
        {
            let mut state = self.session.lock();
            state.tid = tid;
            state.thread_mgr = Some(thread_mgr.clone());
        }

        // 键盘事件 sink 由 TSF 转发到本实例。
        let keys: ITfKeystrokeMgr = thread_mgr.cast()?;
        let key_sink: ITfKeyEventSink = self.to_interface();
        unsafe { keys.AdviseKeyEventSink(tid, &key_sink, true) }?;

        let source: ITfSource = thread_mgr.cast()?;
        let thread_sink: ITfThreadMgrEventSink = self.to_interface();
        let _ = unsafe {
            source.AdviseSink(
                &ITfThreadMgrEventSink::IID,
                &thread_sink,
            )
        };
        let activation: ITfInputProcessorProfileActivationSink = self.to_interface();
        if let Ok(cookie) = unsafe {
            source.AdviseSink(
                &ITfInputProcessorProfileActivationSink::IID,
                &activation,
            )
        } {
            self.activation_cookie
                .store(cookie, std::sync::atomic::Ordering::Relaxed);
        }

        // 编辑路径需要 sink 才能 StartComposition；实例只服务本线程，存一份即可。
        let comp: ITfCompositionSink = self.to_interface();
        *self.composition.lock().unwrap_or_else(|e| e.into_inner()) = Some(comp);
        self.session.lock().composition_sink = self
            .composition
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        if let Ok(doc) = unsafe { thread_mgr.GetFocus() } {
            if let Ok(ctx) = unsafe { doc.GetTop() } {
                self.enter(&ctx);
            }
        }
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for Tip_Impl {
    fn OnSetFocus(&self, _foreground: BOOL) -> Result<()> {
        Ok(())
    }

    fn OnTestKeyDown(&self, _pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = (wparam.0 & 0xFFFF) as u16;
        // Shift 轻按的跟踪放在 Test 回调：它对每个按键都先于 Do 回调到达，
        // 而 Shift 本身不需要被吞，Do 回调不会收到它。
        self.shift
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_down(vk);
        let mods = key::current_mods();
        let eat = !matches!(self.session.decide(vk, mods), Decision::Pass);
        // Shift 单独按下不吃：它要等抬起才知道是不是切换。
        Ok(BOOL::from(eat && vk != VK_SHIFT.0))
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 抬起与按下对称：按下被吞的键，抬起也吞，否则应用会收到孤儿 KeyUp。
        let vk = (wparam.0 & 0xFFFF) as u16;
        if vk == VK_SHIFT.0 {
            // 轻按切换中英：中间没有别的按键才算。
            let tap = self
                .shift
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_up(vk);
            if tap {
                self.toggle_mode();
            }
            return Ok(BOOL::from(true));
        }
        let mods = key::current_mods();
        Ok(BOOL::from(!matches!(
            self.session.decide(vk, mods),
            Decision::Pass
        )))
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let Some(ctx) = pic.cloned() else {
            return Ok(BOOL::from(false));
        };
        let vk = (wparam.0 & 0xFFFF) as u16;

        // 组合期数字键：命中候选则选择，越界则吞掉。
        if let Some(hit) = self.digit_select(vk) {
            return Ok(BOOL::from(match hit {
                Some(index) => self.select(&ctx, index),
                None => true,
            }));
        }
        let decision = self.session.decide(vk, key::current_mods());
        if let Decision::Forward(code) = decision {
            if code == key::KEY_PAGE_UP {
                return Ok(BOOL::from(self.page(&ctx, -1)));
            }
            if code == key::KEY_PAGE_DOWN {
                return Ok(BOOL::from(self.page(&ctx, 1)));
            }
        }
        Ok(BOOL::from(self.key_down(&ctx, wparam, lparam)))
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 引擎只在按下时推进状态机（05）：抬起不发 IPC，吞键决定已在 Test 阶段做完。
        Ok(BOOL::from(true))
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _guid: *const GUID) -> Result<BOOL> {
        Ok(BOOL::from(false))
    }
}

impl ITfThreadMgrEventSink_Impl for Tip_Impl {
    fn OnInitDocumentMgr(&self, _doc: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, _doc: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnSetFocus(
        &self,
        focus: Ref<ITfDocumentMgr>,
        _previous: Ref<ITfDocumentMgr>,
    ) -> Result<()> {
        // 焦点迁移是竞态高发区：先收尾旧会话，再为新文档开会话。
        self.leave();
        if let Some(doc) = focus.cloned() {
            if let Ok(ctx) = unsafe { doc.GetTop() } {
                self.enter(&ctx);
                // text edit sink 挂在 context 上，随焦点切换重挂。
                if let (Ok(source), state) = (
                    ctx.cast::<ITfSource>(),
                    self.session.lock().tid,
                ) {
                    let sink: ITfTextEditSink = self.to_interface();
                    let _ = unsafe { source.AdviseSink(&ITfTextEditSink::IID, &sink) };
                    let _ = state;
                }
            }
        }
        Ok(())
    }

    fn OnPushContext(&self, _ctx: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _ctx: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
}

impl ITfTextEditSink_Impl for Tip_Impl {
    fn OnEndEdit(
        &self,
        _ctx: Ref<ITfContext>,
        _readonly: u32,
        _record: Ref<ITfEditRecord>,
    ) -> Result<()> {
        // 宿主可能在我们之外改文档；此处不追踪外来编辑，等下一键的 host 状态对齐。
        Ok(())
    }
}

impl ITfCompositionSink_Impl for Tip_Impl {
    fn OnCompositionTerminated(
        &self,
        _ec: u32,
        _composition: Ref<windows::Win32::UI::TextServices::ITfComposition>,
    ) -> Result<()> {
        // TSF 之外终止了 composition（宿主抢走、切焦点）：清本地状态，不重入上屏。
        self.session.clear_composition();
        Ok(())
    }
}

impl ITfInputProcessorProfileActivationSink_Impl for Tip_Impl {
    fn OnActivated(
        &self,
        profile_type: u32,
        _langid: u16,
        clsid: *const GUID,
        _catid: *const GUID,
        profile: *const GUID,
        _hkl: HKL,
        flags: u32,
    ) -> Result<()> {
        // 激活本身由 TSF 调 ActivateEx/Deactivate 完成（03），这里只做一件事：
        // 确认是本 profile 的「已停用」通知时，把残留 composition 收干净。
        let mine = !clsid.is_null()
            && unsafe { *clsid } == crate::reg::CLSID_SILK_TIP
            && !profile.is_null()
            && unsafe { *profile } == GUID_SILK_PROFILE;
        if mine && flags & TF_TMF_ACTIVATED == 0 {
            self.session.clear_composition();
        }
        let _ = profile_type;
        Ok(())
    }
}
