package main

import (
	"encoding/json"
	"fmt"
	"log"
	"net"

	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/ipc"
	"github.com/luobochuanqi/silk-ime/internal/session"
)

// 协议消息载荷（线格式契约见 docs/design/foundation/04-process-and-ipc.md）。

type helloMsg struct {
	V     int    `json:"v"`
	Caps  uint32 `json:"caps"`
	Build string `json:"build"`
}

type cfgSnapshot struct {
	PageSize  int  `json:"page_size"`
	Chinese   bool `json:"chinese"`
	FullPunct bool `json:"full_punct"`
}

type welcomeMsg struct {
	V   int         `json:"v"`
	Cfg cfgSnapshot `json:"cfg"`
}

type modeMsg struct {
	Chinese   bool `json:"chinese"`
	FullPunct bool `json:"full_punct"`
}

type focusInMsg struct {
	App      string `json:"app"`
	HWND     uint64 `json:"hwnd"`
	LangHint string `json:"lang_hint"`
}

type keyMsg struct {
	Code uint16 `json:"code"`
	Rune string `json:"rune,omitempty"`
	Down bool   `json:"down"`
	Mods uint8  `json:"mods"`
	TS   uint64 `json:"ts"`
}

type actionMsg struct {
	Kind  string   `json:"kind"` // select | page | toggle | cancel
	Index *int     `json:"index,omitempty"`
	Mode  *modeMsg `json:"mode,omitempty"`
}

type candidateMsg struct {
	Text    string `json:"text"`
	Comment string `json:"comment,omitempty"`
}

type updateMsg struct {
	Preedit    string         `json:"preedit"`
	Caret      int            `json:"caret"`
	Candidates []candidateMsg `json:"candidates"`
	PageIndex  int            `json:"page_index"`
	Mode       modeMsg        `json:"mode"`
}

type commitMsg struct {
	Text string `json:"text"`
}

type errMsg struct {
	ID   uint64 `json:"id"`
	Code int    `json:"code"`
	Msg  string `json:"msg"`
}

type pingMsg struct {
	TS uint64 `json:"ts"`
}

type rectMsg struct {
	X int32 `json:"x"`
	Y int32 `json:"y"`
	W int32 `json:"w"`
	H int32 `json:"h"`
}

type caretMsg struct {
	Rect rectMsg `json:"rect"`
}

// conn 是单条 shim 连接的协议处理器；读循环即该连接全部会话的串行调用方。
type conn struct {
	nc         net.Conn
	mgr        *session.Manager
	cw         *candidateWindow
	snapshot   cfgSnapshot
	handshaked bool
	owner      uintptr
	anchor     silkRect
}

func (c *conn) send(typ, sid string, payload any) error {
	raw, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("编码载荷 %s: %w", typ, err)
	}
	env := ipc.Envelope{V: 1, Type: typ, SID: sid, Payload: raw}
	body, err := ipc.Encode(env)
	if err != nil {
		return fmt.Errorf("编码信封 %s: %w", typ, err)
	}
	return ipc.WriteFrame(c.nc, body)
}

func (c *conn) sendError(sid string, id uint64, code int, msg string) {
	_ = c.send("error", sid, errMsg{ID: id, Code: code, Msg: msg})
}

// run 服务一条连接直到断开或协议违规（畸形帧直接断开，见 04 安全策略）。
func (c *conn) run() {
	defer c.nc.Close()
	for {
		frame, err := ipc.ReadFrame(c.nc)
		if err != nil {
			return
		}
		env, err := ipc.Decode(frame)
		if err != nil {
			log.Printf("解码失败（断开连接）: %v", err)
			return
		}
		if !c.handshaked && env.Type != "hello" && env.Type != "ping" {
			c.sendError(env.SID, env.ID, 400, "未握手")
			return
		}
		log.Printf("收到 %s", env.Type)
		if !c.dispatch(env) {
			return
		}
	}
}

// dispatch 处理单条消息；返回 false 表示连接应关闭。
func (c *conn) dispatch(env ipc.Envelope) bool {
	switch env.Type {
	case "hello":
		var msg helloMsg
		if err := json.Unmarshal(env.Payload, &msg); err != nil || msg.V != 1 {
			c.sendError(env.SID, env.ID, 400, "hello 版本不支持")
			return false
		}
		c.handshaked = true
		_ = c.send("welcome", "", welcomeMsg{V: 1, Cfg: c.snapshot})

	case "focus_in":
		var msg focusInMsg
		if err := json.Unmarshal(env.Payload, &msg); err != nil {
			c.sendError(env.SID, env.ID, 400, "focus_in 载荷非法")
			return true
		}
		c.mgr.FocusIn(env.SID, msg.App)
		c.owner = uintptr(msg.HWND)
		c.sendResponse(env.SID, engine.Response{Mode: engine.Mode{Chinese: c.snapshot.Chinese, FullPunct: c.snapshot.FullPunct}})

	case "caret":
		var msg caretMsg
		if err := json.Unmarshal(env.Payload, &msg); err != nil {
			c.sendError(env.SID, env.ID, 400, "caret 载荷非法")
			return true
		}
		c.anchor = silkRect{x: msg.Rect.X, y: msg.Rect.Y, w: msg.Rect.W, h: msg.Rect.H}

	case "focus_out":
		resp, err := c.mgr.FocusOut(env.SID)
		if err != nil {
			c.sendError(env.SID, env.ID, 404, err.Error())
			return true
		}
		c.sendResponse(env.SID, resp)
		c.cw.hideWindow()

	case "key":
		var msg keyMsg
		if err := json.Unmarshal(env.Payload, &msg); err != nil {
			c.sendError(env.SID, env.ID, 400, "key 载荷非法")
			return true
		}
		resp, err := c.mgr.HandleKey(env.SID, engine.Key{
			Code: engine.KeyCode(msg.Code),
			Rune: firstRune(msg.Rune),
			Down: msg.Down,
			Mods: engine.ModMask(msg.Mods),
			TS:   msg.TS,
		})
		if err != nil {
			c.sendError(env.SID, env.ID, 404, err.Error())
			return true
		}
		c.sendResponse(env.SID, resp)

	case "action":
		var msg actionMsg
		if err := json.Unmarshal(env.Payload, &msg); err != nil {
			c.sendError(env.SID, env.ID, 400, "action 载荷非法")
			return true
		}
		resp, err := c.handleAction(env.SID, msg)
		if err != nil {
			c.sendError(env.SID, env.ID, 400, err.Error())
			return true
		}
		c.sendResponse(env.SID, resp)

	case "ping":
		var msg pingMsg
		_ = json.Unmarshal(env.Payload, &msg)
		_ = c.send("pong", env.SID, pingMsg{TS: msg.TS})

	case "bye":
		return false

	default:
		c.sendError(env.SID, env.ID, 400, "未知消息类型 "+env.Type)
	}
	return true
}

func (c *conn) handleAction(sid string, msg actionMsg) (engine.Response, error) {
	switch msg.Kind {
	case "select":
		if msg.Index == nil {
			return engine.Response{}, fmt.Errorf("select 缺少 index")
		}
		return c.mgr.Select(sid, *msg.Index)
	case "page":
		if msg.Index == nil {
			return engine.Response{}, fmt.Errorf("page 缺少 index")
		}
		return c.mgr.Page(sid, *msg.Index)
	case "toggle":
		if msg.Mode == nil {
			return engine.Response{}, fmt.Errorf("toggle 缺少 mode")
		}
		return c.mgr.Toggle(sid, engine.Mode{Chinese: msg.Mode.Chinese, FullPunct: msg.Mode.FullPunct})
	case "cancel":
		return c.mgr.HandleKey(sid, engine.Key{Code: engine.KeyEsc, Down: true, TS: 0})
	default:
		return engine.Response{}, fmt.Errorf("未知 action kind %q", msg.Kind)
	}
}

// sendResponse 把引擎响应拆为 update（总是）与 commit（有上屏时）两条消息，
// 并驱动候选窗可见性（06 状态机：有候选且 caret 有效显示，commit/空候选/失焦隐藏）。
func (c *conn) sendResponse(sid string, resp engine.Response) {
	cands := make([]candidateMsg, 0, len(resp.Candidates))
	for _, cd := range resp.Candidates {
		cands = append(cands, candidateMsg{Text: cd.Text, Comment: cd.Comment})
	}
	_ = c.send("update", sid, updateMsg{
		Preedit:    resp.Preedit,
		Caret:      resp.Caret,
		Candidates: cands,
		PageIndex:  resp.PageIndex,
		Mode:       modeMsg{Chinese: resp.Mode.Chinese, FullPunct: resp.Mode.FullPunct},
	})
	if resp.Commit != "" {
		_ = c.send("commit", sid, commitMsg{Text: resp.Commit})
		c.cw.hideWindow()
		return
	}
	if len(cands) > 0 {
		c.cw.render(resp.Preedit, cands, resp.PageIndex, c.anchor, c.owner)
		return
	}
	c.cw.hideWindow()
}

func firstRune(s string) rune {
	for _, r := range s {
		return r
	}
	return 0
}
