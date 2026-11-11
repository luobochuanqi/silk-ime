//! TIP 对象：COM 类、ITfTextInputProcessorEx 与四个 sink。
//!
//! 每个文本线程一个实例（Apartment）。所有回调只做三件事：读本地缓存、投递
//! 有界等待的协议请求、把结果落到 composition；业务与 IO 都不在这里。连接建立
//! 在后台线程上（见 client.rs），回调路径从不等待建连（03:83）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use windows::core::{implement, Interface, IUnknownImpl, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{E_FAIL, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetKeyboardState, MapVirtualKeyW, ToUnicodeEx, HKL, MAPVK_VK_TO_VSC, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDocumentMgr, ITfEditRecord,
    ITfInputProcessorProfileActivationSink, ITfInputProcessorProfileActivationSink_Impl,
    ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource, ITfTextEditSink,
    ITfTextEditSink_Impl, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl,
    TF_IPSINK_FLAG_ACTIVE, TF_PROFILETYPE_INPUTPROCESSOR,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use crate::edit;
use crate::key::{self, Decision, EatenKeys, ShiftTap};
use crate::msg;
use crate::session::Session;

/// 本进程内所有实例的存活计数；DllCanUnloadNow 据此回答。
static INSTANCES: AtomicUsize = AtomicUsize::new(0);

pub fn instance_count() -> usize {
    INSTANCES.load(Ordering::Acquire)
}

/// 已登记 sink 的撤销凭据。
///
/// AdviseSink 各自返回 cookie，注销时必须把 cookie 连同登记它的那个接口交回；
/// 丢掉 cookie 就等于泄漏 sink（03:84），所以这里成对保存。
#[derive(Default)]
struct Sinks {
    /// 键盘 sink 走 UnadviseKeyEventSink，只需要登记时的线程 id。
    keys: Option<ITfKeystrokeMgr>,
    tid: u32,
    /// 挂在线程管理器上的 sink（thread mgr / profile activation）。
    advised: Vec<(ITfSource, u32)>,
    /// text edit sink 挂在焦点 context 上：焦点迁移时先撤旧的再挂新的（03:84）。
    edit: Option<(ITfSource, u32)>,
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
    session: Arc<Session>,
    /// 本实例的 composition sink 接口；编辑时回传给 TSF。
    composition: Mutex<Option<ITfCompositionSink>>,
    shift: Mutex<ShiftTap>,
    /// 每个虚拟键在按下时是否被吞；抬起按同一决定处置（04:112 fail-open）。
    eaten: Mutex<EatenKeys>,
    sinks: Mutex<Sinks>,
}

impl Tip {
    pub fn new() -> Tip {
        INSTANCES.fetch_add(1, Ordering::AcqRel);
        Tip {
            session: Session::new(),
            composition: Mutex::new(None),
            shift: Mutex::new(ShiftTap::default()),
            eaten: Mutex::new(EatenKeys::default()),
            sinks: Mutex::new(Sinks::default()),
        }
    }

    fn sinks(&self) -> MutexGuard<'_, Sinks> {
        self.sinks.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn shift(&self) -> MutexGuard<'_, ShiftTap> {
        self.shift.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 记录本次按下的处置结果，供同一按键的抬起复用。
    fn remember_eaten(&self, vk: u16, eaten: bool) {
        self.eaten
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remember(vk, eaten);
    }

    fn peek_eaten(&self, vk: u16) -> bool {
        self.eaten
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .peek(vk)
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
        // 清掉补发用的焦点快照：没有焦点时重连不该凭空造出会话。
        self.session.shared().set_focus(None);
        self.session.clear_composition();
        let mut state = self.session.lock();
        state.context = None;
        state.doc_mgr = None;
        state.sid.clear();
    }

    fn enter(&self, ctx: &ITfContext) {
        // SAFETY: GetForegroundWindow 无参数，返回当前前台窗口句柄（可能为空）。
        let hwnd = unsafe { GetForegroundWindow() };
        let app = process_name(hwnd);
        let sid = self.session.next_sid();
        // SAFETY: ctx 是本线程持有的有效 TSF 接口引用，GetDocumentMgr 只增加引用计数。
        let doc_mgr = unsafe { ctx.GetDocumentMgr() }.ok();
        {
            let mut state = self.session.lock();
            state.context = Some(ctx.clone());
            state.doc_mgr = doc_mgr;
            state.hwnd = hwnd;
            state.sid = sid.clone();
            state.preedit.clear();
            state.candidates.clear();
            state.composition = None;
            state.ui_id = None;
            state.shown = false;
        }
        // 未连上时 focus_in 立即返回 Disconnected：不等待建连（03:83），后台连上后
        // 由 IO 线程按这份快照补发。
        let hwnd_raw = hwnd.0 as isize as u64;
        self.session.shared().set_focus(Some(crate::client::Focus {
            sid: sid.clone(),
            app: app.clone(),
            hwnd: hwnd_raw,
        }));
        // 只唤醒后台建连线程，不在这里等它（03:83 回调 15 ms 上限）。
        self.session.warm();
        let env = msg::focus_in(0, &sid, &app, hwnd_raw);
        if let crate::client::Reply::Messages(messages) = self.session.request(&env, true) {
            for intent in self.session.absorb(&messages) {
                let _ = edit::apply(&self.session, &intent);
            }
        }
    }

    /// 撤销全部 sink 登记。Deactivate 只调用一次，重复调用无副作用。
    fn release_sinks(&self) {
        let (keys, tid, mut advised) = {
            let mut sinks = self.sinks();
            (
                sinks.keys.take(),
                std::mem::take(&mut sinks.tid),
                std::mem::take(&mut sinks.advised),
            )
        };
        advised.extend(self.sinks().edit.take());
        if let Some(keys) = keys {
            // SAFETY: keys 是登记键盘 sink 时用的同一线程管理器，tid 也原样保存；
            // 注销只按 tid 摘除登记，不涉及外部内存。
            unsafe {
                let _ = keys.UnadviseKeyEventSink(tid);
            }
        }
        for (source, cookie) in advised {
            // SAFETY: source 是当初返回该 cookie 的同一接口实例，cookie 原样交回。
            unsafe {
                let _ = source.UnadviseSink(cookie);
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
                    let eaten =
                        !sid.is_empty() && self.session.handle_action(&msg::action_cancel(0, &sid));
                    crate::ui::sync(&self.session, ctx);
                    return eaten;
                }
                let ts = crate::client::now_ms();
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
        INSTANCES.fetch_sub(1, Ordering::AcqRel);
    }
}

fn process_name(hwnd: windows::Win32::Foundation::HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    // SAFETY: hwnd 由 GetForegroundWindow 给出或为空（已在上面判空）；pid 是本栈
    // 上的可写量，函数只写它。
    let pid = unsafe {
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid
    };
    if pid == 0 {
        return String::new();
    }
    // SAFETY: OpenProcess 只按 pid 取句柄；PROCESS_QUERY_LIMITED_INFORMATION 足够
    // 读进程映像名且不需要更高权限。
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
    // SAFETY: handle 有效；buf 是 512 个 u16 的独占缓冲，size 初值等于其容量，
    // 内核最多写这么多字符。
    let ok = unsafe {
        windows::Win32::System::Threading::QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: 句柄由本函数独占，关闭即释放；之后不再使用。
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    if ok.is_err() {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..size as usize]);
    path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned()
}

/// 用当前键盘布局把 VK 译成字符；标点只需要它来填 rune 载荷。
fn char_from_key(vk: u16, _lparam: LPARAM) -> Option<char> {
    let mut state = [0u8; 256];
    // SAFETY: state 是 256 字节的独占缓冲，GetKeyboardState 只会写这么多。
    unsafe { GetKeyboardState(&mut state) }.ok()?;
    // SAFETY: MapVirtualKeyW/GetKeyboardLayout 都是无副作用的纯查询。
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    // SAFETY: GetKeyboardLayout(0) 取当前线程的布局句柄，参数只是线程 id。
    let layout: HKL = unsafe { GetKeyboardLayout(0) };
    let mut buf = [0u16; 8];
    // SAFETY: buf 容量 8 个 u16，返回值是写入长度；state 与 layout 在调用期间有效。
    let len = unsafe { ToUnicodeEx(vk as u32, scan as u32, &state, &mut buf, 0, Some(layout)) };
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
        self.release_sinks();
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for Tip_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, _flags: u32) -> Result<()> {
        let Some(thread_mgr) = ptim.cloned() else {
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
        // SAFETY: keys 是本线程的键盘管理器；key_sink 是本对象的接口引用，TSF 持有
        // 它直到 UnadviseKeyEventSink；tid 来自 ActivateEx。
        unsafe { keys.AdviseKeyEventSink(tid, &key_sink, true) }?;

        let source: ITfSource = thread_mgr.cast()?;
        let thread_sink: ITfThreadMgrEventSink = self.to_interface();
        let thread_cookie =
            // SAFETY: source 属于本线程的管理器；sink 引用由本对象持有，cookie 记录后
            // 供 Deactivate 撤销（03:84）。
            unsafe { source.AdviseSink(&ITfThreadMgrEventSink::IID, &thread_sink) };
        let activation: ITfInputProcessorProfileActivationSink = self.to_interface();
        let profile_cookie =
            // SAFETY: 同上，sink 引用同样由本对象持有。
            unsafe { source.AdviseSink(&ITfInputProcessorProfileActivationSink::IID, &activation) };

        {
            let mut sinks = self.sinks();
            sinks.keys = Some(keys);
            sinks.tid = tid;
            for cookie in [thread_cookie, profile_cookie].into_iter().flatten() {
                sinks.advised.push((source.clone(), cookie));
            }
        }
        // 编辑路径需要 sink 才能 StartComposition；实例只服务本线程，存一份即可。
        let comp: ITfCompositionSink = self.to_interface();
        *self.composition.lock().unwrap_or_else(|e| e.into_inner()) = Some(comp.clone());
        self.session.lock().composition_sink = Some(comp);

        // SAFETY: thread_mgr 有效；GetFocus 返回文档管理器或失败，不涉及裸指针。
        if let Ok(doc) = unsafe { thread_mgr.GetFocus() } {
            // SAFETY: doc 是刚取得的有效接口引用。
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
        self.shift().on_down(vk);
        let mods = key::current_mods();
        let eat = !matches!(self.session.decide(vk, mods), Decision::Pass);
        // 放行的键不会进 OnKeyDown，这里就把记账清掉：否则失焦漏掉抬起后残留的旧值
        // 会让下一次抬起被误吞。
        if !eat || vk == VK_SHIFT.0 {
            self.remember_eaten(vk, false);
        }
        // Shift 单独按下不吃：它要等抬起才知道是不是切换。
        Ok(BOOL::from(eat && vk != VK_SHIFT.0))
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = (wparam.0 & 0xFFFF) as u16;
        if vk == VK_SHIFT.0 {
            // 轻按切换中英：中间没有别的按键才算。抬起放行——按下时本就没吞，
            // 吞抬起只会让宿主看到孤儿 KeyUp。
            if self.shift().on_up(vk) {
                self.toggle_mode();
            }
            return Ok(BOOL::from(false));
        }
        // 与按下时的实际处置一致（04:112）：只有按下被吞的键才吞抬起。Test 阶段只
        // 查询，记录在 OnKeyUp 里清掉。
        Ok(BOOL::from(self.peek_eaten(vk)))
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let Some(ctx) = pic.cloned() else {
            return Ok(BOOL::from(false));
        };
        let vk = (wparam.0 & 0xFFFF) as u16;

        // 组合期数字键：命中候选则选择，越界则吞掉。
        if let Some(hit) = self.digit_select(vk) {
            let eaten = match hit {
                Some(index) => self.select(&ctx, index),
                None => true,
            };
            self.remember_eaten(vk, eaten);
            return Ok(BOOL::from(eaten));
        }
        let decision = self.session.decide(vk, key::current_mods());
        if let Decision::Forward(code) = decision {
            if code == key::KEY_PAGE_UP || code == key::KEY_PAGE_DOWN {
                let eaten = self.page(&ctx, if code == key::KEY_PAGE_UP { -1 } else { 1 });
                self.remember_eaten(vk, eaten);
                return Ok(BOOL::from(eaten));
            }
        }
        let eaten = self.key_down(&ctx, wparam, lparam);
        self.remember_eaten(vk, eaten);
        Ok(BOOL::from(eaten))
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 引擎只在按下时推进状态机（05）：抬起不发 IPC。TSF 用本回调的 *pfEaten 决定
        // 是否吞键，所以这里必须给出与 Test 阶段相同的答案，否则孤儿 KeyUp 会漏给应用。
        let vk = (wparam.0 & 0xFFFF) as u16;
        if vk == VK_SHIFT.0 {
            return Ok(BOOL::from(false));
        }
        let eaten = self.peek_eaten(vk);
        // 一次按下对应一次抬起：读完后立刻清掉，避免残留值影响下一次。
        self.remember_eaten(vk, false);
        Ok(BOOL::from(eaten))
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
        // text edit sink 挂在 context 上：先撤旧的，避免每换一次焦点多挂一份。
        if let Some((source, cookie)) = self.sinks().edit.take() {
            // SAFETY: source 是当初登记该 cookie 的接口实例，撤销不触及外部内存。
            unsafe {
                let _ = source.UnadviseSink(cookie);
            }
        }
        if let Some(doc) = focus.cloned() {
            // SAFETY: doc 是 TSF 传入的有效文档管理器引用。
            if let Ok(ctx) = unsafe { doc.GetTop() } {
                self.enter(&ctx);
                if let Ok(source) = ctx.cast::<ITfSource>() {
                    let sink: ITfTextEditSink = self.to_interface();
                    if let Ok(cookie) =
                        // SAFETY: source 属于刚取得的 context；sink 引用由本对象持有，
                        // cookie 记录后供下次焦点迁移或 Deactivate 撤销。
                        unsafe { source.AdviseSink(&ITfTextEditSink::IID, &sink) }
                    {
                        self.sinks().edit = Some((source, cookie));
                    }
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
        // 激活/停用的判定位是 TF_IPSINK_FLAG_ACTIVE（msctf.h 的 dwFlags），不是
        // TF_TMF_ACTIVATED —— 后者属于 ITfThreadMgr 的启动标志，语义无关。
        // 键盘布局 profile 的 clsid/profile 是 NULL，按有效指针过滤即可。
        let mine = profile_type == TF_PROFILETYPE_INPUTPROCESSOR
            && !clsid.is_null()
            && !profile.is_null()
            // SAFETY: 两个指针由 TSF 按接口契约传入，非空已在上面确认。
            && unsafe { *clsid == crate::reg::CLSID_SILK_TIP }
            // SAFETY: 同上，profile 非空已确认。
            && unsafe { *profile == crate::reg::GUID_SILK_PROFILE };
        // 本 profile 被切走：把残留 composition 收干净（激活本身由 TSF 调
        // ActivateEx/Deactivate 完成，03「激活感知」）。
        if mine && flags & TF_IPSINK_FLAG_ACTIVE == 0 {
            self.session.clear_composition();
        }
        Ok(())
    }
}
