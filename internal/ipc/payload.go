package ipc

// 消息载荷的 Go 结构，字段与 schema.go 及 04-process-and-ipc.md 的载荷表逐项对应。

// Hello 是 shim 首连接的握手载荷。PID 为可选字段，供 host 日志关联对端进程。
type Hello struct {
	V     int    `json:"v"`
	Caps  uint32 `json:"caps"`
	Build string `json:"build"`
	PID   uint32 `json:"pid,omitempty"`
}

// Config 是 welcome 下发的全局配置快照。
type Config struct {
	PageSize  int  `json:"page_size"`
	Chinese   bool `json:"chinese"`
	FullPunct bool `json:"full_punct"`
}

// Welcome 是握手应答。
type Welcome struct {
	V   int    `json:"v"`
	Cfg Config `json:"cfg"`
}

// Mode 是输入模式。
type Mode struct {
	Chinese   bool `json:"chinese"`
	FullPunct bool `json:"full_punct"`
}

// FocusIn 是焦点进入载荷。
type FocusIn struct {
	App      string `json:"app"`
	HWND     uint64 `json:"hwnd"`
	LangHint string `json:"lang_hint"`
}

// Key 是语义键事件。
type Key struct {
	Code uint16 `json:"code"`
	Rune string `json:"rune,omitempty"`
	Down bool   `json:"down"`
	Mods uint8  `json:"mods"`
	TS   uint64 `json:"ts"`
}

// Action 动作种类。
const (
	ActionSelect = "select"
	ActionPage   = "page"
	ActionToggle = "toggle"
	ActionCancel = "cancel"
)

// Action 是选择/翻页/模式切换/取消；Index 用于 select（0 起）与 page（±1）。
type Action struct {
	Kind  string `json:"kind"`
	Index *int   `json:"index,omitempty"`
	Mode  *Mode  `json:"mode,omitempty"`
}

// Candidate 是候选项。
type Candidate struct {
	Text    string `json:"text"`
	Comment string `json:"comment,omitempty"`
}

// Update 是会话状态的全量快照。同一请求的应答中 update 总是最后一帧，
// shim 以「id 匹配的 update 或 error」判定应答结束。
type Update struct {
	Preedit    string      `json:"preedit"`
	Caret      int         `json:"caret"`
	Candidates []Candidate `json:"candidates"`
	PageIndex  int         `json:"page_index"`
	Mode       Mode        `json:"mode"`
}

// Commit 是上屏文本。
type Commit struct {
	Text string `json:"text"`
}

// Error 是请求失败应答。
type Error struct {
	ID   uint64 `json:"id"`
	Code int    `json:"code"`
	Msg  string `json:"msg"`
}

// 错误码沿用 HTTP 语义，便于日志阅读。
const (
	CodeBadRequest     = 400
	CodeUnknownSession = 404
)

// Ping 是存活探测，pong 原样回带 TS。
type Ping struct {
	TS uint64 `json:"ts"`
}

// Rect 是屏幕坐标矩形（物理像素）。
type Rect struct {
	X int32 `json:"x"`
	Y int32 `json:"y"`
	W int32 `json:"w"`
	H int32 `json:"h"`
}

// Caret 是插入点位置。
type Caret struct {
	Rect Rect `json:"rect"`
}
