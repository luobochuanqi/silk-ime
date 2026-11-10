//! silk-tsftip.dll 与 silkhost.exe 之间的协议编解码：帧格式、JSON 封套与语义
//! 校验。协议定义见 docs/design/foundation/04-process-and-ipc.md。
//!
//! 解码器面对不可信输入：任何字节序列都只返回错误，不会 panic。与 Go 侧
//! internal/ipc 共享 protocol/fixtures 下的同一批样例。

use std::fmt;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 当前协议版本。host 兼容 N-1，shim 遇到更高版本必须拒绝并降级。
pub const VERSION: u32 = 1;

/// 帧体上限（1 MiB）。超出即视为攻击或流错位，接收方必须断开连接，不得截断后继续。
pub const MAX_FRAME_SIZE: usize = 1 << 20;

pub const TYPE_HELLO: &str = "hello";
pub const TYPE_WELCOME: &str = "welcome";
pub const TYPE_FOCUS_IN: &str = "focus_in";
pub const TYPE_FOCUS_OUT: &str = "focus_out";
pub const TYPE_KEY: &str = "key";
pub const TYPE_ACTION: &str = "action";
pub const TYPE_UPDATE: &str = "update";
pub const TYPE_COMMIT: &str = "commit";
pub const TYPE_CARET: &str = "caret";
pub const TYPE_SHOW: &str = "show";
pub const TYPE_HIDE: &str = "hide";
pub const TYPE_PING: &str = "ping";
pub const TYPE_PONG: &str = "pong";
pub const TYPE_ERROR: &str = "error";
pub const TYPE_BYE: &str = "bye";

/// 解码或编码失败的类别。
#[derive(Debug)]
pub enum Error {
    /// 声明的帧长超过 [`MAX_FRAME_SIZE`]。
    FrameTooLarge(usize),
    /// 长度前缀、封套或载荷不满足协议语义。
    InvalidMessage(String),
    /// 底层 I/O 失败。
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::FrameTooLarge(n) => write!(f, "ipc: 帧长超过上限: {n} 字节"),
            Error::InvalidMessage(msg) => write!(f, "ipc: 消息无效: {msg}"),
            Error::Io(err) => write!(f, "ipc: I/O 失败: {err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

/// 帧体内的 JSON 封套。
///
/// `id` 为请求-响应关联号，host 主动下行事件为 0。`sid` 在 focus_in 时由 host
/// 分配，会话级消息必须携带非空值。`payload` 必须是 JSON 对象，字段按类型校验，
/// 未知字段被忽略以保持 N-1 兼容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub id: u64,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    pub payload: Value,
}

impl Envelope {
    /// 校验封套与载荷语义。
    pub fn validate(&self) -> Result<(), Error> {
        if self.v != VERSION {
            return Err(invalid(format!("不支持的协议版本 {}", self.v)));
        }
        let spec = spec_for(&self.type_)
            .ok_or_else(|| invalid(format!("未知消息类型 {:?}", self.type_)))?;
        if spec.require_sid && self.sid.as_deref().unwrap_or("").is_empty() {
            return Err(invalid(format!("{} 缺少 sid", self.type_)));
        }
        let obj = self
            .payload
            .as_object()
            .ok_or_else(|| invalid(format!("{} 的载荷必须是 JSON 对象", self.type_)))?;
        validate_object(obj, spec.payload).map_err(|msg| invalid(format!("{}: {msg}", self.type_)))
    }
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidMessage(msg.into())
}

#[derive(Clone, Copy)]
enum Kind {
    String,
    Number,
    Bool,
    Object,
    Array,
}

struct Field {
    kind: Kind,
    required: bool,
    /// Object 或 Array（元素为对象）时的嵌套字段。
    schema: &'static [(&'static str, Field)],
}

impl Field {
    const fn required(kind: Kind) -> Self {
        Field {
            kind,
            required: true,
            schema: &[],
        }
    }
    const fn optional(kind: Kind) -> Self {
        Field {
            kind,
            required: false,
            schema: &[],
        }
    }
    const fn required_with(kind: Kind, schema: &'static [(&'static str, Field)]) -> Self {
        Field {
            kind,
            required: true,
            schema,
        }
    }
    const fn optional_with(kind: Kind, schema: &'static [(&'static str, Field)]) -> Self {
        Field {
            kind,
            required: false,
            schema,
        }
    }
}

struct MessageSpec {
    require_sid: bool,
    payload: &'static [(&'static str, Field)],
}

const CANDIDATE_FIELDS: &[(&str, Field)] = &[
    ("text", Field::required(Kind::String)),
    ("comment", Field::optional(Kind::String)),
];

const MODE_FIELDS: &[(&str, Field)] = &[
    ("chinese", Field::required(Kind::Bool)),
    ("full_punct", Field::required(Kind::Bool)),
];

const RECT_FIELDS: &[(&str, Field)] = &[
    ("x", Field::required(Kind::Number)),
    ("y", Field::required(Kind::Number)),
    ("w", Field::required(Kind::Number)),
    ("h", Field::required(Kind::Number)),
];

const EMPTY: &[(&str, Field)] = &[];

/// 消息语义的唯一事实来源：类型未知即拒绝，缺失必填字段即拒绝。
fn spec_for(type_: &str) -> Option<MessageSpec> {
    const HELLO: &[(&str, Field)] = &[
        ("v", Field::required(Kind::Number)),
        ("caps", Field::required(Kind::Number)),
        ("build", Field::required(Kind::String)),
    ];
    const WELCOME: &[(&str, Field)] = &[
        ("v", Field::required(Kind::Number)),
        ("cfg", Field::required(Kind::Object)),
    ];
    const FOCUS_IN: &[(&str, Field)] = &[
        ("app", Field::required(Kind::String)),
        ("hwnd", Field::required(Kind::Number)),
        ("lang_hint", Field::required(Kind::String)),
    ];
    const KEY: &[(&str, Field)] = &[
        ("code", Field::required(Kind::Number)),
        ("rune", Field::optional(Kind::String)),
        ("down", Field::required(Kind::Bool)),
        ("mods", Field::required(Kind::Number)),
        ("ts", Field::required(Kind::Number)),
    ];
    const ACTION: &[(&str, Field)] = &[
        ("kind", Field::required(Kind::String)),
        ("index", Field::optional(Kind::Number)),
        ("mode", Field::optional_with(Kind::Object, MODE_FIELDS)),
    ];
    const UPDATE: &[(&str, Field)] = &[
        ("preedit", Field::required(Kind::String)),
        ("caret", Field::required(Kind::Number)),
        (
            "candidates",
            Field::required_with(Kind::Array, CANDIDATE_FIELDS),
        ),
        ("page_index", Field::required(Kind::Number)),
        ("mode", Field::required_with(Kind::Object, MODE_FIELDS)),
    ];
    const COMMIT: &[(&str, Field)] = &[("text", Field::required(Kind::String))];
    const CARET: &[(&str, Field)] = &[("rect", Field::required_with(Kind::Object, RECT_FIELDS))];
    const SHOW: &[(&str, Field)] = &[("view", Field::optional(Kind::Object))];
    const PING: &[(&str, Field)] = &[("ts", Field::required(Kind::Number))];
    const ERROR: &[(&str, Field)] = &[
        ("id", Field::required(Kind::Number)),
        ("code", Field::required(Kind::Number)),
        ("msg", Field::required(Kind::String)),
    ];

    let (require_sid, payload) = match type_ {
        TYPE_HELLO => (false, HELLO),
        TYPE_WELCOME => (false, WELCOME),
        TYPE_FOCUS_IN => (false, FOCUS_IN),
        TYPE_KEY => (true, KEY),
        TYPE_ACTION => (true, ACTION),
        TYPE_UPDATE => (true, UPDATE),
        TYPE_COMMIT => (true, COMMIT),
        TYPE_CARET => (true, CARET),
        TYPE_SHOW => (true, SHOW),
        TYPE_PING => (false, PING),
        TYPE_PONG => (false, PING),
        TYPE_ERROR => (false, ERROR),
        TYPE_FOCUS_OUT | TYPE_HIDE | TYPE_BYE => (true, EMPTY),
        _ => return None,
    };
    Some(MessageSpec {
        require_sid,
        payload,
    })
}

fn validate_object(
    obj: &serde_json::Map<String, Value>,
    schema: &[(&str, Field)],
) -> Result<(), String> {
    for (name, field) in schema {
        match obj.get(*name) {
            None => {
                if field.required {
                    return Err(format!("缺少字段 {name:?}"));
                }
            }
            Some(value) => {
                validate_field(value, field).map_err(|msg| format!("字段 {name:?}: {msg}"))?
            }
        }
    }
    Ok(())
}

fn validate_field(value: &Value, field: &Field) -> Result<(), String> {
    let matches = match field.kind {
        Kind::String => value.is_string(),
        Kind::Number => value.is_number(),
        Kind::Bool => value.is_boolean(),
        Kind::Object => value.is_object(),
        Kind::Array => value.is_array(),
    };
    if !matches {
        return Err("类型不匹配".into());
    }
    match (field.kind, value) {
        (Kind::Object, Value::Object(obj)) => validate_object(obj, field.schema),
        (Kind::Array, Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                let obj = item
                    .as_object()
                    .ok_or_else(|| format!("第 {index} 项不是对象"))?;
                validate_object(obj, field.schema)
                    .map_err(|msg| format!("第 {index} 项: {msg}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// 将 `payload` 作为一帧写入 `w`：u32 小端长度前缀 + 帧体。
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> Result<(), Error> {
    if payload.len() > MAX_FRAME_SIZE {
        return Err(Error::FrameTooLarge(payload.len()));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    Ok(())
}

/// 从 `r` 读取一帧并返回帧体。
///
/// 长度前缀超限返回 [`Error::FrameTooLarge`]；此时流位置已不可恢复，调用方必须断开。
pub fn read_frame(r: &mut impl Read) -> Result<Vec<u8>, Error> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header)?;
    let n = u32::from_le_bytes(header) as usize;
    if n > MAX_FRAME_SIZE {
        return Err(Error::FrameTooLarge(n));
    }
    let mut payload = vec![0u8; n];
    r.read_exact(&mut payload)?;
    Ok(payload)
}

/// 校验封套后编码为整帧（长度前缀 + JSON）。
pub fn encode(env: &Envelope) -> Result<Vec<u8>, Error> {
    env.validate()?;
    let body = serde_json::to_vec(env).map_err(|err| invalid(format!("序列化封套: {err}")))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    write_frame(&mut frame, &body)?;
    Ok(frame)
}

/// 解析整帧并做语义校验。任何输入都只产生错误，不会 panic。
pub fn decode(frame: &[u8]) -> Result<Envelope, Error> {
    if frame.len() < 4 {
        return Err(invalid("帧短于长度前缀"));
    }
    let n = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if n > MAX_FRAME_SIZE {
        return Err(Error::FrameTooLarge(n));
    }
    let body = &frame[4..];
    if n != body.len() {
        return Err(invalid(format!("长度前缀 {n} 与实际 {} 不符", body.len())));
    }
    if std::str::from_utf8(body).is_err() {
        return Err(invalid("帧体不是合法 UTF-8"));
    }
    let env: Envelope =
        serde_json::from_slice(body).map_err(|err| invalid(format!("封套解析失败: {err}")))?;
    env.validate()?;
    Ok(env)
}
