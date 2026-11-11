//! 协议消息的构造与解析。
//!
//! 只做数据成形与校验：不碰管道、不碰 COM，便于单测。字段名与
//! docs/design/foundation/04-process-and-ipc.md 的 payload 表逐项对应。

use serde::Deserialize;
use serde_json::{json, Value};
use silk_protocol::{
    Envelope, TYPE_ACTION, TYPE_BYE, TYPE_CARET, TYPE_FOCUS_IN, TYPE_FOCUS_OUT, TYPE_HELLO,
    TYPE_KEY, TYPE_PING, TYPE_UPDATE, TYPE_WELCOME, VERSION,
};

/// TIP 构建号，进入 hello，便于 host 侧诊断混装版本。
pub const BUILD: &str = concat!("tsftip-", env!("CARGO_PKG_VERSION"));

/// 协议能力位。当前没有可选能力，保留字段以便加法演进。
pub const CAPS: u32 = 0;

/// 候选与模式是 host 下行状态的两块核心数据。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Candidate {
    pub text: String,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ModePayload {
    pub chinese: bool,
    pub full_punct: bool,
}

/// `update` 的载荷。缺字段按默认值处理而不是整条丢弃：状态机上少一次更新
/// 只是走一帧旧状态，比断开连接安全。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UpdatePayload {
    #[serde(default)]
    pub preedit: String,
    #[serde(default)]
    pub caret: i32,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub page_index: i32,
    #[serde(default)]
    pub mode: Option<ModePayload>,
}

/// `welcome` 的全局配置快照。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WelcomePayload {
    #[serde(default)]
    pub v: u32,
    #[serde(default)]
    pub cfg: WelcomeCfg,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WelcomeCfg {
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub chinese: bool,
    #[serde(default)]
    pub full_punct: bool,
}

/// 屏幕矩形，`caret` 消息的载荷。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

fn envelope(id: u64, type_: &str, sid: Option<&str>, payload: Value) -> Envelope {
    Envelope {
        v: VERSION,
        id,
        type_: type_.to_owned(),
        sid: sid.map(str::to_owned),
        payload,
    }
}

pub fn hello() -> Envelope {
    envelope(0, TYPE_HELLO, None, json!({ "v": VERSION, "caps": CAPS, "build": BUILD }))
}

pub fn focus_in(id: u64, sid: &str, app: &str, hwnd: u64) -> Envelope {
    envelope(
        id,
        TYPE_FOCUS_IN,
        Some(sid),
        json!({ "app": app, "hwnd": hwnd, "lang_hint": "zh-CN" }),
    )
}

pub fn focus_out(id: u64, sid: &str) -> Envelope {
    envelope(id, TYPE_FOCUS_OUT, Some(sid), json!({}))
}

pub fn bye(sid: &str) -> Envelope {
    envelope(0, TYPE_BYE, Some(sid), json!({}))
}

pub fn key(id: u64, sid: &str, code: u16, rune: Option<char>, down: bool, mods: u8, ts: u64) -> Envelope {
    let mut payload = json!({ "code": code, "down": down, "mods": mods, "ts": ts });
    if let Some(rune) = rune {
        payload["rune"] = Value::String(rune.to_string());
    }
    envelope(id, TYPE_KEY, Some(sid), payload)
}

/// 空闲存活探测（04:72，30 s 一次）；host 原样回 `pong`，本层不需要处理回应。
pub fn ping(ts: u64) -> Envelope {
    envelope(0, TYPE_PING, None, json!({ "ts": ts }))
}

/// `action` 的四种语义：选择、翻页、模式切换、取消。
pub fn action_select(id: u64, sid: &str, index: u32) -> Envelope {
    envelope(id, TYPE_ACTION, Some(sid), json!({ "kind": "select", "index": index }))
}

pub fn action_page(id: u64, sid: &str, delta: i32) -> Envelope {
    envelope(id, TYPE_ACTION, Some(sid), json!({ "kind": "page", "index": delta }))
}

pub fn action_toggle(id: u64, sid: &str, chinese: bool, full_punct: bool) -> Envelope {
    envelope(
        id,
        TYPE_ACTION,
        Some(sid),
        json!({ "kind": "toggle", "mode": { "chinese": chinese, "full_punct": full_punct } }),
    )
}

pub fn action_cancel(id: u64, sid: &str) -> Envelope {
    envelope(id, TYPE_ACTION, Some(sid), json!({ "kind": "cancel" }))
}

pub fn caret(id: u64, sid: &str, rect: Rect) -> Envelope {
    envelope(
        id,
        TYPE_CARET,
        Some(sid),
        json!({ "rect": { "x": rect.x, "y": rect.y, "w": rect.w, "h": rect.h } }),
    )
}

/// 一条下行消息解析后的语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Welcome(WelcomeCfg),
    Update(UpdatePayload),
    Commit(String),
    Error { code: i64, msg: String },
    /// ping/pong/未知类型：本层不需要动作。
    Other,
}

/// 解析下行消息。载荷畸形按 Other 处理，调用方不因此断开（host 侧已保证
/// 合法封套，两次校验都失败说明版本错位，等重连即可）。
pub fn parse_incoming(env: &Envelope) -> Incoming {
    match env.type_.as_str() {
        TYPE_WELCOME => serde_json::from_value::<WelcomePayload>(env.payload.clone())
            .map(|w| Incoming::Welcome(w.cfg))
            .unwrap_or(Incoming::Other),
        TYPE_UPDATE => serde_json::from_value::<UpdatePayload>(env.payload.clone())
            .map(Incoming::Update)
            .unwrap_or(Incoming::Other),
        "commit" => env
            .payload
            .get("text")
            .and_then(Value::as_str)
            .map(|text| Incoming::Commit(text.to_owned()))
            .unwrap_or(Incoming::Other),
        "error" => Incoming::Error {
            code: env.payload.get("code").and_then(Value::as_i64).unwrap_or(0),
            msg: env
                .payload
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        _ => Incoming::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(env: &Envelope) -> Value {
        env.payload.clone()
    }

    #[test]
    fn hello_carries_version_and_build() {
        let env = hello();
        assert_eq!(env.v, VERSION);
        assert_eq!(env.type_, "hello");
        assert_eq!(env.sid, None);
        assert_eq!(roundtrip(&env)["caps"], 0);
        assert_eq!(roundtrip(&env)["build"], BUILD);
        env.validate().expect("hello 合法");
    }

    #[test]
    fn focus_in_uses_lang_hint_and_sid() {
        let env = focus_in(7, "s-1", "notepad.exe", 0x1234);
        assert_eq!(env.sid.as_deref(), Some("s-1"));
        assert_eq!(env.payload["lang_hint"], "zh-CN");
        assert_eq!(env.payload["app"], "notepad.exe");
        assert_eq!(env.payload["hwnd"], 0x1234);
        env.validate().expect("focus_in 合法");
    }

    #[test]
    fn key_omits_missing_rune() {
        let plain = key(1, "s-1", 8, None, true, 0, 42);
        assert!(plain.payload.get("rune").is_none());
        assert_eq!(plain.payload["code"], 8);
        assert_eq!(plain.payload["ts"], 42);
        let punct = key(2, "s-1", 43, Some('，'), true, 1, 43);
        assert_eq!(punct.payload["rune"], "，");
        plain.validate().expect("key 合法");
        punct.validate().expect("key 合法");
    }

    #[test]
    fn actions_match_schema() {
        let select = action_select(3, "s-1", 2);
        assert_eq!(select.payload["kind"], "select");
        assert_eq!(select.payload["index"], 2);
        let page = action_page(4, "s-1", -1);
        assert_eq!(page.payload["index"], -1);
        let toggle = action_toggle(5, "s-1", false, true);
        assert_eq!(toggle.payload["mode"]["chinese"], false);
        assert_eq!(toggle.payload["mode"]["full_punct"], true);
        let cancel = action_cancel(6, "s-1");
        assert_eq!(cancel.payload["kind"], "cancel");
        for env in [&select, &page, &toggle, &cancel] {
            env.validate().expect("action 合法");
        }
    }

    #[test]
    fn caret_carries_rect() {
        let env = caret(
            8,
            "s-1",
            Rect {
                x: 10,
                y: 20,
                w: 2,
                h: 16,
            },
        );
        assert_eq!(env.payload["rect"]["h"], 16);
        env.validate().expect("caret 合法");
    }

    #[test]
    fn ping_carries_timestamp() {
        let env = ping(1234);
        assert_eq!(env.type_, TYPE_PING);
        assert_eq!(env.payload["ts"], 1234);
        env.validate().expect("ping 合法");
    }

    #[test]
    fn parses_update_and_commit() {
        let update = Envelope {
            v: VERSION,
            id: 0,
            type_: TYPE_UPDATE.to_owned(),
            sid: Some("s-1".to_owned()),
            payload: json!({
                "preedit": "hao",
                "caret": 3,
                "candidates": [{"text": "好"}, {"text": "号", "comment": "hào"}],
                "page_index": 1,
                "mode": {"chinese": true, "full_punct": false}
            }),
        };
        update.validate().expect("update 合法");
        match parse_incoming(&update) {
            Incoming::Update(u) => {
                assert_eq!(u.preedit, "hao");
                assert_eq!(u.caret, 3);
                assert_eq!(u.candidates.len(), 2);
                assert_eq!(u.candidates[1].comment.as_deref(), Some("hào"));
                assert_eq!(u.page_index, 1);
                assert_eq!(
                    u.mode,
                    Some(ModePayload {
                        chinese: true,
                        full_punct: false
                    })
                );
            }
            other => panic!("应解析为 update，实际 {other:?}"),
        }

        let commit = Envelope {
            v: VERSION,
            id: 0,
            type_: "commit".to_owned(),
            sid: Some("s-1".to_owned()),
            payload: json!({ "text": "你好" }),
        };
        assert_eq!(parse_incoming(&commit), Incoming::Commit("你好".to_owned()));
    }

    #[test]
    fn parses_welcome_and_error() {
        let welcome = Envelope {
            v: VERSION,
            id: 0,
            type_: TYPE_WELCOME.to_owned(),
            sid: None,
            payload: json!({"v": 1, "cfg": {"page_size": 5, "chinese": true, "full_punct": true}}),
        };
        match parse_incoming(&welcome) {
            Incoming::Welcome(cfg) => {
                assert_eq!(cfg.page_size, 5);
                assert!(cfg.chinese);
            }
            other => panic!("应解析为 welcome，实际 {other:?}"),
        }

        let error = Envelope {
            v: VERSION,
            id: 9,
            type_: "error".to_owned(),
            sid: None,
            payload: json!({"id": 9, "code": 404, "msg": "未知会话"}),
        };
        assert_eq!(
            parse_incoming(&error),
            Incoming::Error {
                code: 404,
                msg: "未知会话".to_owned()
            }
        );
    }

    #[test]
    fn bye_and_focus_out_require_sid() {
        let env = bye("s-1");
        assert_eq!(env.payload, json!({}));
        env.validate().expect("bye 合法");
        let env = focus_out(2, "s-1");
        env.validate().expect("focus_out 合法");
        // 缺 sid 的会话级消息必须被协议层拒绝。
        let mut bad = bye("s-1");
        bad.sid = None;
        assert!(bad.validate().is_err());
    }
}
