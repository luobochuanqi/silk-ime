// Package ipc 实现 silk-tsftip.dll 与 silkhost.exe 之间的协议编解码：帧格式、
// JSON 封套与消息语义校验。协议定义见
// docs/design/foundation/04-process-and-ipc.md。
//
// 解码器是不可信输入边界：任何字节序列都必须返回 Envelope 或错误，不得 panic。
package ipc

import "encoding/json"

// Version 是当前协议版本。host 兼容 N-1，shim 遇到更高版本必须拒绝并降级。
const Version = 1

// 消息类型目录。方向与载荷语义见 04-process-and-ipc.md；目录只允许加法演进，
// 未知类型一律视为协议错误，不能按默认语义处理。
const (
	TypeHello    = "hello"
	TypeWelcome  = "welcome"
	TypeFocusIn  = "focus_in"
	TypeFocusOut = "focus_out"
	TypeKey      = "key"
	TypeAction   = "action"
	TypeUpdate   = "update"
	TypeCommit   = "commit"
	TypeCaret    = "caret"
	TypeShow     = "show"
	TypeHide     = "hide"
	TypePing     = "ping"
	TypePong     = "pong"
	TypeError    = "error"
	TypeBye      = "bye"
)

// Envelope 是帧体内的 JSON 封套。
//
// ID 为请求-响应关联号，host 主动下行事件为 0。SID 在 focus_in 时由 host
// 分配，会话级消息必须携带非空值。Payload 必须是 JSON 对象，字段按类型校验；
// 未知字段被忽略以保持 N-1 兼容。
type Envelope struct {
	V       int             `json:"v"`
	ID      uint64          `json:"id"`
	Type    string          `json:"type"`
	SID     string          `json:"sid,omitempty"`
	Payload json.RawMessage `json:"payload"`
}

// wireEnvelope 用指针区分"字段缺失"与"零值"：缺失必填字段必须报错。
type wireEnvelope struct {
	V       *int            `json:"v"`
	ID      *uint64         `json:"id"`
	Type    *string         `json:"type"`
	SID     string          `json:"sid"`
	Payload json.RawMessage `json:"payload"`
}
