//! 一次 TIP 实例的会话状态：焦点、composition、候选、协议客户端。
//!
//! TSF 回调全部发生在宿主的文本线程上（Apartment），因此状态用一把互斥锁串行化：
//! 跨对象共享（composition sink、候选 UI 元素）需要 Arc，锁保证回调重入安全。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use silk_protocol::Envelope;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfContext, ITfDocumentMgr, ITfThreadMgr,
};

use crate::client::{Client, Reply, Shared};
use crate::key::{should_consume, Decision, Mode};
use crate::msg::{self, Candidate, Incoming, UpdatePayload};

/// 每线程一个 TIP 实例；sid 在实例内递增，host 用它区分文档会话。
// Arc 只在本线程（Apartment）内分发，绝不跨线程：State 持有 TSF 接口指针，
// 线程间传递本身就是 COM 错误。
#[allow(clippy::arc_with_non_send_sync)]
pub struct Session {
    client: Client,
    shared: Arc<Shared>,
    sid_counter: AtomicU32,
    pub state: Mutex<State>,
}

/// 会话的可变状态。字段全部只在 TSF 回调临界区内读写。
#[derive(Default)]
pub struct State {
    pub tid: u32,
    pub thread_mgr: Option<ITfThreadMgr>,
    pub context: Option<ITfContext>,
    pub doc_mgr: Option<ITfDocumentMgr>,
    pub hwnd: HWND,
    /// 本实例的 composition sink（Tip 自身），编辑时回传给 TSF。
    pub composition_sink: Option<windows::Win32::UI::TextServices::ITfCompositionSink>,
    /// 当前 sid；空串表示没有焦点会话。
    pub sid: String,
    pub composition: Option<ITfComposition>,
    pub preedit: String,
    pub candidates: Vec<Candidate>,
    pub page_index: u32,
    pub selected: u32,
    /// 候选窗已登记的 UI 元素 id 与共享内容。
    pub ui_id: Option<u32>,
    pub ui_view: Option<crate::ui::SharedView>,
    pub shown: bool,
}

impl State {
    pub fn composing(&self) -> bool {
        !self.preedit.is_empty()
    }
}

/// 一次按键处理的结果：是否吞掉这次按键。
pub struct KeyOutcome {
    pub eaten: bool,
}

impl Session {
    /// 只在本线程内使用：State 持有 TSF 接口指针，不允许跨 Apartment 传递。
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new() -> Arc<Session> {
        let client = Client::new();
        let shared = client.shared();
        Arc::new(Session {
            client,
            shared,
            sid_counter: AtomicU32::new(0),
            state: Mutex::new(State::default()),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn mode(&self) -> Mode {
        self.shared.mode()
    }

    pub fn page_size(&self) -> u32 {
        self.shared.page_size()
    }

    /// 全新 sid：`s-` + 实例内递增序号。
    pub fn next_sid(&self) -> String {
        format!("s-{}", self.sid_counter.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// 发送会话请求并等待最多 15 ms。返回下行消息序列。
    pub fn request(&self, env: &Envelope, want_reply: bool) -> Reply {
        self.client.request(env, want_reply, crate::client::KEY_TIMEOUT)
    }

    /// 不等待回应的消息（caret/ping/bye）。
    pub fn post(&self, env: &Envelope) {
        self.client.post(env);
    }

    /// 把下行消息转成编辑意图，并同步模式缓存。
    pub fn absorb(&self, messages: &[Incoming]) -> Vec<EditIntent> {
        let mut intents = Vec::new();
        for incoming in messages {
            match incoming {
                Incoming::Update(update) => {
                    // 模式以 host 为权威，缓存在本地供下一键零延迟决策。
                    if let Some(mode) = update.mode {
                        self.shared.set_mode(mode.chinese, mode.full_punct);
                    }
                    intents.push(EditIntent::Update(update.clone()));
                }
                Incoming::Commit(text) => intents.push(EditIntent::Commit(text.clone())),
                Incoming::Welcome(cfg) => {
                    self.shared.set_mode(cfg.chinese, cfg.full_punct);
                }
                _ => {}
            }
        }
        intents
    }

    /// 记录一次 host 响应后的状态：候选与 preedit 是下一键判定的依据。
    pub fn apply_update(&self, update: &UpdatePayload) {
        let mut state = self.lock();
        state.preedit = update.preedit.clone();
        state.candidates = update.candidates.clone();
        state.page_index = update.page_index.max(0) as u32;
        state.selected = 0;
        if update.candidates.is_empty() {
            state.ui_id = None;
            state.shown = false;
        }
    }

    pub fn clear_composition(&self) {
        let mut state = self.lock();
        state.composition = None;
        state.preedit.clear();
        state.candidates.clear();
        state.page_index = 0;
        state.selected = 0;
        state.ui_id = None;
        state.ui_view = None;
        state.shown = false;
    }

    /// 处理一次按键，含 15 ms 超时降级。
    ///
    /// 超时或断连时返回 eaten=false：按键原样放行，绝不吞键（04 降级矩阵）。
    pub fn handle_key(
        self: &Arc<Self>,
        code: u16,
        rune: Option<char>,
        down: bool,
        mods: u8,
        ts: u64,
    ) -> KeyOutcome {
        let had = { self.lock().composing() };
        let sid = { self.lock().sid.clone() };
        // 没有焦点会话：放行，按键由应用自己处理。
        if sid.is_empty() {
            return KeyOutcome { eaten: false };
        }
        let env = msg::key(0, &sid, code, rune, down, mods, ts);
        let reply = self.request(&env, true);
        let plan = plan_from_reply(&reply, had, code);
        if plan.passthrough {
            return KeyOutcome { eaten: false };
        }
        let intents = self.absorb(&plan.messages);
        for intent in &intents {
            if !self.commit_to_edit(intent) {
                // 编辑失败（只读文档等）：本次按键放行，不当成已吞。
                return KeyOutcome { eaten: false };
            }
        }
        KeyOutcome { eaten: plan.eaten }
    }

    /// 选择/翻页/取消等动作不进入按键通道，同样带硬超时。
    pub fn handle_action(self: &Arc<Self>, env: &Envelope) -> bool {
        match self.request(env, true) {
            Reply::Messages(messages) => {
                let intents = self.absorb(&messages);
                intents.iter().all(|intent| self.commit_to_edit(intent))
            }
            Reply::Timeout | Reply::Disconnected => false,
        }
    }

    /// 把编辑意图提交给 TSF。渲染或编辑失败只降级隐藏，不向调用方抛出。
    fn commit_to_edit(self: &Arc<Self>, intent: &EditIntent) -> bool {
        match crate::edit::apply(self, intent) {
            Ok(()) => true,
            Err(_) => {
                self.lock().candidates.clear();
                false
            }
        }
    }

    pub fn decide(&self, vk: u16, mods: u8) -> Decision {
        let composing = self.lock().composing();
        crate::key::decide(vk, mods, self.mode(), composing)
    }
}

/// 一次请求结果对按键的处置。
///
/// 超时与断连一律放行：按键永远先于状态（04 降级矩阵），迟到响应由 IO 线程丢弃。
pub struct KeyPlan {
    pub messages: Vec<Incoming>,
    pub eaten: bool,
    pub passthrough: bool,
}

pub fn plan_from_reply(reply: &Reply, had_composition: bool, code: u16) -> KeyPlan {
    match reply {
        Reply::Messages(messages) => {
            let mut update: Option<&UpdatePayload> = None;
            let mut commit = "";
            for message in messages {
                match message {
                    Incoming::Update(value) => update = Some(value),
                    Incoming::Commit(text) => commit = text.as_str(),
                    _ => {}
                }
            }
            let preedit = update.map(|u| u.preedit.as_str()).unwrap_or("");
            KeyPlan {
                eaten: should_consume(preedit, commit, had_composition, code),
                messages: messages.clone(),
                passthrough: false,
            }
        }
        Reply::Timeout | Reply::Disconnected => KeyPlan {
            messages: Vec::new(),
            eaten: false,
            passthrough: true,
        },
    }
}

/// 一次 host 响应中需要落到 composition 的编辑意图。
#[derive(Debug, Clone)]
pub enum EditIntent {
    Update(UpdatePayload),
    Commit(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{Candidate, ModePayload};

    fn update(preedit: &str) -> Incoming {
        Incoming::Update(UpdatePayload {
            preedit: preedit.to_owned(),
            caret: preedit.chars().count() as i32,
            candidates: vec![Candidate {
                text: "好".to_owned(),
                comment: None,
            }],
            page_index: 0,
            mode: Some(ModePayload {
                chinese: true,
                full_punct: true,
            }),
        })
    }

    #[test]
    fn timeout_passes_key_through() {
        for reply in [Reply::Timeout, Reply::Disconnected] {
            let plan = plan_from_reply(&reply, false, crate::key::KEY_A);
            assert!(plan.passthrough, "超时/断连必须放行");
            assert!(!plan.eaten);
            assert!(plan.messages.is_empty());
        }
    }

    #[test]
    fn preedit_consumes_key() {
        let reply = Reply::Messages(vec![update("h")]);
        let plan = plan_from_reply(&reply, false, crate::key::KEY_A);
        assert!(!plan.passthrough);
        assert!(plan.eaten);
        assert_eq!(plan.messages.len(), 1);
    }

    #[test]
    fn commit_consumes_key_even_without_preedit() {
        let reply = Reply::Messages(vec![update(""), Incoming::Commit("好".to_owned())]);
        let plan = plan_from_reply(&reply, true, crate::key::KEY_SPACE);
        assert!(plan.eaten);
    }

    #[test]
    fn empty_result_passes_unless_editing_key() {
        let reply = Reply::Messages(vec![update("")]);
        assert!(!plan_from_reply(&reply, false, crate::key::KEY_SPACE).eaten);
        assert!(plan_from_reply(&reply, true, crate::key::KEY_ESC).eaten);
        assert!(!plan_from_reply(&reply, true, crate::key::KEY_A).eaten);
    }

    #[test]
    fn absorb_applies_mode_from_update() {
        let session = Session::new();
        let mut shared = session.shared().mode();
        assert!(shared.chinese);
        let messages = vec![Incoming::Update(UpdatePayload {
            mode: Some(ModePayload {
                chinese: false,
                full_punct: false,
            }),
            ..Default::default()
        })];
        let intents = session.absorb(&messages);
        assert_eq!(intents.len(), 1);
        shared = session.mode();
        assert!(!shared.chinese);
        assert!(!shared.full_punct);
    }

    #[test]
    fn absorb_orders_update_before_commit() {
        let session = Session::new();
        let intents = session.absorb(&[update("hao"), Incoming::Commit("好".to_owned())]);
        assert!(matches!(intents[0], EditIntent::Update(_)));
        assert!(matches!(intents[1], EditIntent::Commit(_)));
    }

    #[test]
    fn sid_increments_per_instance() {
        let session = Session::new();
        assert_eq!(session.next_sid(), "s-1");
        assert_eq!(session.next_sid(), "s-2");
    }

    #[test]
    fn english_mode_decision_is_passthrough() {
        let session = Session::new();
        session.shared().set_mode(false, true);
        assert_eq!(
            session.decide(windows::Win32::UI::Input::KeyboardAndMouse::VK_A.0, 0),
            Decision::Pass
        );
    }
}
