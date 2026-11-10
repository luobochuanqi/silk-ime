// Package engine 实现输入法的纯逻辑内核：按键流 → 预编辑、候选页与上屏文本。
//
// 包内不含文件、网络、窗口或平台 API，也不读取时钟；Handle 的响应只由会话状态
// 与事件决定，因此同一事件序列重放必定得到相同结果。词库由调用方以只读字节喂入，
// 用户词频以增量形式交给运行时落盘。
package engine

// ModMask 是修饰键位掩码。基座不分区修饰键语义，原样透传。
type ModMask uint8

// KeyCode 是平台无关的语义键码。宿主负责把虚拟键翻译到这一层。
type KeyCode uint16

const (
	KeyA KeyCode = iota + 1
	KeyB
	KeyC
	KeyD
	KeyE
	KeyF
	KeyG
	KeyH
	KeyI
	KeyJ
	KeyK
	KeyL
	KeyM
	KeyN
	KeyO
	KeyP
	KeyQ
	KeyR
	KeyS
	KeyT
	KeyU
	KeyV
	KeyW
	KeyX
	KeyY
	KeyZ
)

const (
	Key0 KeyCode = iota + 1 + KeyZ
	Key1
	Key2
	Key3
	Key4
	Key5
	Key6
	Key7
	Key8
	Key9
)

const (
	KeySpace KeyCode = iota + 1 + Key9
	KeyEnter
	KeyBackspace
	KeyEsc
	KeyPageUp
	KeyPageDown
	KeyPunct
)

// Key 是一次按键事件。Down 为 false 表示抬起，引擎只在按下时驱动状态机。
type Key struct {
	Code KeyCode
	Rune rune
	Down bool
	Mods ModMask
	TS   uint64
}

// Mode 是输入模式快照。
type Mode struct {
	Chinese   bool
	FullPunct bool
}

// Candidate 是候选页中的一项，Comment 为展示用的补充说明。
type Candidate struct {
	Text    string
	Comment string
}

// Event 是会话收到的唯一输入。四个可选字段至多一个非空；Key 与其余字段互斥。
type Event struct {
	Key      *Key
	Select   *int
	Page     *int
	Toggle   *Mode
	FocusOut bool
}

// Response 是一次事件的处理结果。Preedit 为空表示当前没有组合。
type Response struct {
	Consume    bool
	Preedit    string
	Caret      int
	Candidates []Candidate
	PageIndex  int
	Commit     string
	Mode       Mode
}

// UserEntry 是一条用户词频记录，拼音为规范全拼音节。
type UserEntry struct {
	Word   string
	Pinyin string
	Freq   int
	TS     int64
}

// SessionMeta 描述会话所属应用，基座不据此改变行为，仅为后续按应用统计留位。
type SessionMeta struct {
	App string
}

// Config 是引擎级配置，Open 之后不可修改。PageSize 为 0 时取 5；模糊音组内互
// 等价，仅在音节首/尾生效。EnglishCompletion 为后续英文单词补全预留，v1 不实现。
type Config struct {
	PageSize          int
	FuzzyGroups       [][]string
	EnglishCompletion bool
}
