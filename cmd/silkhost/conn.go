package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"runtime/debug"
	"time"

	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/ipc"
	"github.com/luobochuanqi/silk-ime/internal/logging"
	"github.com/luobochuanqi/silk-ime/internal/session"
)

// slowRequest 是单次请求处理的告警阈值：shim 的硬超时是 30 ms，host 侧占用超过
// 三分之一预算就值得在默认级别留痕。
const slowRequest = 10 * time.Millisecond

// conn 是单条 shim 连接的协议处理器。读循环是该连接全部会话的唯一调用方，
// 除 ui（跨连接共享）外的字段都无需加锁。
//
// 应答约定（04）：同一请求的应答帧都回带请求 id，先 commit（如有）后 update；
// update 或 error 是应答的最后一帧，shim 据此判定应答结束，不依赖时序。
type conn struct {
	id       uint64
	nc       net.Conn
	mgr      *session.Manager
	client   *session.Client
	ui       *presenter
	pageSize int
	log      *slog.Logger

	handshaked bool

	// 候选窗锚点：caret 消息在按键应答之后才到，有候选但锚点未就绪的视图
	// 暂存在 view，等 caret 到达再显示（否则每次组字的首键都不出候选窗）。
	hwnd   uintptr
	anchor silkRect
	view   *candidateView
}

func newConn(id uint64, nc net.Conn, mgr *session.Manager, ui *presenter, pageSize int) *conn {
	return &conn{
		id:       id,
		nc:       nc,
		mgr:      mgr,
		client:   mgr.NewClient(),
		ui:       ui,
		pageSize: pageSize,
		log:      slog.With("conn", id),
	}
}

// requestError 是可回报给 shim 的请求失败。
type requestError struct {
	code int
	msg  string
}

func (e *requestError) Error() string { return e.msg }

func badRequest(format string, args ...any) error {
	return &requestError{code: ipc.CodeBadRequest, msg: fmt.Sprintf(format, args...)}
}

// run 服务一条连接直到断开或协议违规（畸形帧直接断开，见 04 安全策略）。
// 返回前回收本连接的全部会话与候选窗归属。
func (c *conn) run() {
	start := time.Now()
	frames := 0
	reason := "对端关闭"
	defer func() {
		if r := recover(); r != nil {
			reason = "panic"
			c.log.Error("连接处理崩溃", "panic", r, "stack", string(debug.Stack()))
		}
		orphans := c.client.Len()
		c.client.Close()
		c.ui.release(c.id)
		c.nc.Close()
		c.log.Info("连接关闭", "reason", reason, "frames", frames, "orphan_sessions", orphans,
			"uptime", time.Since(start).Round(time.Second))
	}()

	for {
		frame, err := ipc.ReadFrame(c.nc)
		if err != nil {
			if !errors.Is(err, io.EOF) {
				reason = err.Error()
			}
			return
		}
		frames++
		env, err := ipc.Decode(frame)
		if err != nil {
			reason = "畸形帧"
			c.log.Warn("解码失败，断开连接", "err", err)
			return
		}
		if !c.handshaked && env.Type != ipc.TypeHello && env.Type != ipc.TypePing {
			reason = "未握手"
			c.fail(env, badRequest("未握手"))
			return
		}
		if !c.dispatch(env) {
			reason = "握手失败"
			return
		}
	}
}

// dispatch 处理单条消息；返回 false 表示连接应关闭。
func (c *conn) dispatch(env ipc.Envelope) bool {
	switch env.Type {
	case ipc.TypeHello:
		return c.hello(env)
	case ipc.TypePing:
		var msg ipc.Ping
		_ = json.Unmarshal(env.Payload, &msg)
		c.send(ipc.TypePong, env, msg)
	case ipc.TypeCaret:
		c.caret(env)
	case ipc.TypeBye:
		// bye 只结束其会话：连接由 TIP 实例跨焦点复用，断开会让下一键白白直通。
		c.client.End(env.SID)
		c.ui.hide(c.key(env.SID))
	case ipc.TypeFocusIn, ipc.TypeFocusOut, ipc.TypeKey, ipc.TypeAction:
		start := time.Now()
		resp, err := c.request(env)
		if err != nil {
			c.fail(env, err)
			return true
		}
		c.reply(env, resp)
		c.trace(env, resp, time.Since(start))
	default:
		c.fail(env, badRequest("host 不接受 %s 消息", env.Type))
	}
	return true
}

func (c *conn) hello(env ipc.Envelope) bool {
	var msg ipc.Hello
	if err := json.Unmarshal(env.Payload, &msg); err != nil || msg.V != ipc.Version {
		c.fail(env, badRequest("hello 版本不支持"))
		return false
	}
	if !c.handshaked {
		c.handshaked = true
		c.log = c.log.With("pid", msg.PID)
		c.log.Info("握手完成", "build", msg.Build, "caps", msg.Caps)
	}
	mode := c.mgr.Mode()
	c.send(ipc.TypeWelcome, env, ipc.Welcome{V: ipc.Version, Cfg: ipc.Config{
		PageSize:  c.pageSize,
		Chinese:   mode.Chinese,
		FullPunct: mode.FullPunct,
	}})
	return true
}

// request 把会话级请求交给引擎。
func (c *conn) request(env ipc.Envelope) (engine.Response, error) {
	switch env.Type {
	case ipc.TypeFocusIn:
		msg, err := decode[ipc.FocusIn](env)
		if err != nil {
			return engine.Response{}, err
		}
		c.hwnd, c.anchor, c.view = uintptr(msg.HWND), silkRect{}, nil
		c.log.Debug("焦点进入", "sid", env.SID, "app", msg.App)
		return c.client.FocusIn(env.SID, msg.App), nil
	case ipc.TypeFocusOut:
		return c.client.FocusOut(env.SID)
	case ipc.TypeKey:
		msg, err := decode[ipc.Key](env)
		if err != nil {
			return engine.Response{}, err
		}
		return c.client.Handle(env.SID, engine.Event{Key: &engine.Key{
			Code: engine.KeyCode(msg.Code),
			Rune: firstRune(msg.Rune),
			Down: msg.Down,
			Mods: engine.ModMask(msg.Mods),
			TS:   msg.TS,
		}})
	case ipc.TypeAction:
		msg, err := decode[ipc.Action](env)
		if err != nil {
			return engine.Response{}, err
		}
		ev, err := actionEvent(msg)
		if err != nil {
			return engine.Response{}, err
		}
		return c.client.Handle(env.SID, ev)
	}
	return engine.Response{}, badRequest("非会话请求 %s", env.Type)
}

func actionEvent(msg ipc.Action) (engine.Event, error) {
	switch msg.Kind {
	case ipc.ActionSelect:
		if msg.Index == nil {
			return engine.Event{}, badRequest("select 缺少 index")
		}
		return engine.Event{Select: msg.Index}, nil
	case ipc.ActionPage:
		if msg.Index == nil {
			return engine.Event{}, badRequest("page 缺少 index")
		}
		return engine.Event{Page: msg.Index}, nil
	case ipc.ActionToggle:
		if msg.Mode == nil {
			return engine.Event{}, badRequest("toggle 缺少 mode")
		}
		return engine.Event{Toggle: &engine.Mode{Chinese: msg.Mode.Chinese, FullPunct: msg.Mode.FullPunct}}, nil
	case ipc.ActionCancel:
		return engine.Event{Key: &engine.Key{Code: engine.KeyEsc, Down: true}}, nil
	}
	return engine.Event{}, badRequest("未知 action kind %q", msg.Kind)
}

// reply 发出应答（commit 在前、update 收尾）并驱动候选窗可见性。
func (c *conn) reply(env ipc.Envelope, resp engine.Response) {
	cands := make([]ipc.Candidate, len(resp.Candidates))
	for i, cd := range resp.Candidates {
		cands[i] = ipc.Candidate{Text: cd.Text, Comment: cd.Comment}
	}
	if resp.Commit != "" {
		c.send(ipc.TypeCommit, env, ipc.Commit{Text: resp.Commit})
	}
	c.send(ipc.TypeUpdate, env, ipc.Update{
		Preedit:    resp.Preedit,
		Caret:      resp.Caret,
		Candidates: cands,
		PageIndex:  resp.PageIndex,
		Mode:       ipc.Mode{Chinese: resp.Mode.Chinese, FullPunct: resp.Mode.FullPunct},
	})

	k := c.key(env.SID)
	if len(cands) == 0 || env.Type == ipc.TypeFocusOut {
		c.view = nil
		c.ui.hide(k)
		return
	}
	c.view = &candidateView{sid: env.SID, preedit: resp.Preedit, candidates: cands, pageIndex: resp.PageIndex}
	// 06：坐标未就绪（无有效 caret）时不在错误位置显示，等 caret 到达。
	if c.anchor.w > 0 && c.anchor.h > 0 {
		c.ui.show(k, c.view, c.anchor, c.hwnd)
	}
}

// caret 更新锚点：窗口已显示则原子移动（一次 SetWindowPos，不闪烁），
// 有待显示的视图则就地显示。
func (c *conn) caret(env ipc.Envelope) {
	msg, err := decode[ipc.Caret](env)
	if err != nil {
		c.log.Warn("caret 载荷非法", "err", err)
		return
	}
	c.anchor = silkRect{x: msg.Rect.X, y: msg.Rect.Y, w: msg.Rect.W, h: msg.Rect.H}
	if c.view == nil || c.view.sid != env.SID || c.anchor.w <= 0 || c.anchor.h <= 0 {
		return
	}
	k := c.key(env.SID)
	if !c.ui.move(k, c.anchor) {
		c.ui.show(k, c.view, c.anchor, c.hwnd)
	}
}

func (c *conn) key(sid string) viewKey {
	return viewKey{conn: c.id, sid: sid}
}

// send 发送一帧应答，回带请求的 id 与 sid。写失败意味着连接已断，读循环随后退出。
func (c *conn) send(typ string, req ipc.Envelope, payload any) {
	raw, err := json.Marshal(payload)
	if err == nil {
		var frame []byte
		frame, err = ipc.Encode(ipc.Envelope{V: ipc.Version, ID: req.ID, Type: typ, SID: req.SID, Payload: raw})
		if err == nil {
			err = ipc.WriteFrame(c.nc, frame)
		}
	}
	if err != nil {
		c.log.Warn("发送失败", "type", typ, "id", req.ID, "err", err)
	}
}

func (c *conn) fail(env ipc.Envelope, err error) {
	code := ipc.CodeBadRequest
	var re *requestError
	switch {
	case errors.As(err, &re):
		code = re.code
	case errors.Is(err, session.ErrUnknownSession):
		code = ipc.CodeUnknownSession
	}
	c.log.Warn("请求失败", "type", env.Type, "sid", env.SID, "id", env.ID, "code", code, "err", err)
	c.send(ipc.TypeError, env, ipc.Error{ID: env.ID, Code: code, Msg: err.Error()})
}

// trace 记录单次请求：debug 级别逐条记录，超过 slowRequest 时以 warn 留痕。
func (c *conn) trace(env ipc.Envelope, resp engine.Response, dur time.Duration) {
	level := slog.LevelDebug
	if dur >= slowRequest {
		level = slog.LevelWarn
	}
	if !c.log.Enabled(context.Background(), level) {
		return
	}
	attrs := []any{
		"type", env.Type, "sid", env.SID, "id", env.ID, "dur", dur,
		logging.Text("preedit", resp.Preedit), "cands", len(resp.Candidates), "page", resp.PageIndex,
	}
	if resp.Commit != "" {
		attrs = append(attrs, logging.Text("commit", resp.Commit))
	}
	c.log.Log(context.Background(), level, "请求", attrs...)
}

func decode[T any](env ipc.Envelope) (T, error) {
	var v T
	if err := json.Unmarshal(env.Payload, &v); err != nil {
		return v, badRequest("%s 载荷非法: %v", env.Type, err)
	}
	return v, nil
}

func firstRune(s string) rune {
	for _, r := range s {
		return r
	}
	return 0
}
