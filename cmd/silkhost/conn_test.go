package main

import (
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/luobochuanqi/silk-ime/internal/dict"
	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/ipc"
	"github.com/luobochuanqi/silk-ime/internal/session"
)

// 端到端协议测试：以 net.Pipe 模拟 shim，走真实词库/引擎/会话栈，
// 断言握手、候选产生与上屏的线行为（不依赖 Windows 与渲染）。

func newTestManager(t *testing.T) *session.Manager {
	t.Helper()
	src, err := os.ReadFile(filepath.Join("..", "..", "dict", "source.tsv"))
	if err != nil {
		t.Fatalf("读取词库源: %v", err)
	}
	sources, err := dict.ParseSource(src)
	if err != nil {
		t.Fatalf("解析词库源: %v", err)
	}
	data, err := dict.Build(sources)
	if err != nil {
		t.Fatalf("构建词库: %v", err)
	}
	eng, err := engine.Open(data, nil, engine.Config{PageSize: 5})
	if err != nil {
		t.Fatalf("引擎: %v", err)
	}
	return session.NewManager(eng, "")
}

type testPipe struct {
	t    *testing.T
	conn net.Conn
	done chan struct{}
	next uint64
}

func dial(t *testing.T, id uint64, mgr *session.Manager) *testPipe {
	t.Helper()
	client, server := net.Pipe()
	p := &testPipe{t: t, conn: client, done: make(chan struct{})}
	go func() {
		newConn(id, server, mgr, &presenter{}, 5).run()
		close(p.done)
	}()
	t.Cleanup(func() { client.Close(); <-p.done })
	return p
}

// send 发一条请求并返回其 id。
func (p *testPipe) send(typ, sid string, payload any) uint64 {
	p.t.Helper()
	p.next++
	raw, err := json.Marshal(payload)
	if err != nil {
		p.t.Fatalf("载荷编码: %v", err)
	}
	frame, err := ipc.Encode(ipc.Envelope{V: 1, ID: p.next, Type: typ, SID: sid, Payload: raw})
	if err != nil {
		p.t.Fatalf("编码 %s: %v", typ, err)
	}
	if err := ipc.WriteFrame(p.conn, frame); err != nil {
		p.t.Fatalf("发送 %s: %v", typ, err)
	}
	return p.next
}

func (p *testPipe) recv() ipc.Envelope {
	p.t.Helper()
	frame, err := ipc.ReadFrame(p.conn)
	if err != nil {
		p.t.Fatalf("接收: %v", err)
	}
	env, err := ipc.Decode(frame)
	if err != nil {
		p.t.Fatalf("解码: %v", err)
	}
	return env
}

// roundtrip 发一条会话请求，收齐应答（直到 update/error），断言每帧回带请求 id。
func (p *testPipe) roundtrip(typ, sid string, payload any) []ipc.Envelope {
	p.t.Helper()
	id := p.send(typ, sid, payload)
	var frames []ipc.Envelope
	for {
		env := p.recv()
		if env.ID != id {
			p.t.Fatalf("%s 应答 id=%d，期望 %d", env.Type, env.ID, id)
		}
		frames = append(frames, env)
		if env.Type == ipc.TypeUpdate || env.Type == ipc.TypeError {
			return frames
		}
	}
}

func (p *testPipe) handshake() {
	p.t.Helper()
	p.send(ipc.TypeHello, "", ipc.Hello{V: 1, Build: "test", PID: 1})
	if env := p.recv(); env.Type != ipc.TypeWelcome {
		p.t.Fatalf("期望 welcome，得到 %s", env.Type)
	}
}

func (p *testPipe) typeKeys(sid string, codes ...uint16) ipc.Update {
	p.t.Helper()
	var last []ipc.Envelope
	for _, code := range codes {
		last = p.roundtrip(ipc.TypeKey, sid, ipc.Key{Code: code, Down: true, TS: 100})
	}
	return decodeUpdate(p.t, last[len(last)-1])
}

func decodeUpdate(t *testing.T, env ipc.Envelope) ipc.Update {
	t.Helper()
	if env.Type != ipc.TypeUpdate {
		t.Fatalf("期望 update，得到 %s: %s", env.Type, env.Payload)
	}
	var u ipc.Update
	if err := json.Unmarshal(env.Payload, &u); err != nil {
		t.Fatalf("update 载荷: %v", err)
	}
	return u
}

// 键码：N=14 I=9 H=8 A=1 O=15，Space=37。
var nihao = []uint16{14, 9, 8, 1, 15}

func TestHandshakeAndTyping(t *testing.T) {
	p := dial(t, 1, newTestManager(t))
	p.handshake()
	p.roundtrip(ipc.TypeFocusIn, "s-1", ipc.FocusIn{App: "notepad.exe", HWND: 42, LangHint: "zh-CN"})
	if u := p.typeKeys("s-1", nihao...); u.Preedit != "nihao" || len(u.Candidates) == 0 {
		t.Fatalf("组字状态错误: %+v", u)
	}

	frames := p.roundtrip(ipc.TypeKey, "s-1", ipc.Key{Code: 37, Down: true, TS: 200})
	if len(frames) != 2 || frames[0].Type != ipc.TypeCommit {
		t.Fatalf("空格上屏应答应为 commit+update，得到 %d 帧", len(frames))
	}
	var c ipc.Commit
	if err := json.Unmarshal(frames[0].Payload, &c); err != nil || c.Text != "你好" {
		t.Fatalf("上屏应为 你好: %s", frames[0].Payload)
	}
	if u := decodeUpdate(t, frames[1]); u.Preedit != "" {
		t.Fatalf("上屏后 preedit 应清空: %+v", u)
	}
}

func TestUnhandshakedRejected(t *testing.T) {
	p := dial(t, 1, newTestManager(t))
	p.send(ipc.TypeKey, "s-1", ipc.Key{Code: 14, Down: true, TS: 1})
	if env := p.recv(); env.Type != ipc.TypeError {
		t.Fatalf("未握手发消息应报错，得到 %s", env.Type)
	}
}

func TestUnknownSession(t *testing.T) {
	p := dial(t, 1, newTestManager(t))
	p.handshake()
	frames := p.roundtrip(ipc.TypeKey, "s-x", ipc.Key{Code: 14, Down: true, TS: 1})
	var e ipc.Error
	if err := json.Unmarshal(frames[0].Payload, &e); err != nil || e.Code != ipc.CodeUnknownSession {
		t.Fatalf("未知会话应报 404: %s", frames[0].Payload)
	}
}

// 各 shim 实例独立编号 sid：两个进程都用 "s-1" 时会话不得串扰。
func TestSessionsIsolatedPerConnection(t *testing.T) {
	mgr := newTestManager(t)
	a, b := dial(t, 1, mgr), dial(t, 2, mgr)
	for _, p := range []*testPipe{a, b} {
		p.handshake()
		p.roundtrip(ipc.TypeFocusIn, "s-1", ipc.FocusIn{App: "app.exe"})
	}
	a.typeKeys("s-1", 14, 9) // ni
	if u := b.typeKeys("s-1", 8); u.Preedit != "h" {
		t.Fatalf("连接 b 的组字被连接 a 污染: %q", u.Preedit)
	}
	if u := a.typeKeys("s-1", 8); u.Preedit != "nih" {
		t.Fatalf("连接 a 的组字被连接 b 污染: %q", u.Preedit)
	}
}

// bye 只结束会话，连接继续服务后续焦点。
func TestByeKeepsConnection(t *testing.T) {
	p := dial(t, 1, newTestManager(t))
	p.handshake()
	p.roundtrip(ipc.TypeFocusIn, "s-1", ipc.FocusIn{App: "app.exe"})
	p.send(ipc.TypeBye, "s-1", struct{}{})
	p.roundtrip(ipc.TypeFocusIn, "s-2", ipc.FocusIn{App: "app.exe"})
	if u := p.typeKeys("s-2", 14); u.Preedit != "n" {
		t.Fatalf("bye 之后连接应可继续使用: %+v", u)
	}
}

// 连接断开必须回收会话，否则活动计数不归零，host 永不空闲退出。
func TestDisconnectReleasesSessions(t *testing.T) {
	mgr := newTestManager(t)
	p := dial(t, 1, mgr)
	p.handshake()
	p.roundtrip(ipc.TypeFocusIn, "s-1", ipc.FocusIn{App: "app.exe"})
	if n := mgr.ActiveSessions(); n != 1 {
		t.Fatalf("活动会话应为 1，得到 %d", n)
	}
	p.conn.Close()
	select {
	case <-p.done:
	case <-time.After(time.Second):
		t.Fatal("连接未退出")
	}
	if n := mgr.ActiveSessions(); n != 0 {
		t.Fatalf("断开后活动会话应为 0，得到 %d", n)
	}
}

// 模式跨会话共享：切到英文后新焦点仍是英文，welcome 也如实下发。
func TestModeSharedAcrossSessions(t *testing.T) {
	mgr := newTestManager(t)
	p := dial(t, 1, mgr)
	p.handshake()
	p.roundtrip(ipc.TypeFocusIn, "s-1", ipc.FocusIn{App: "a.exe"})
	p.roundtrip(ipc.TypeAction, "s-1", ipc.Action{Kind: ipc.ActionToggle, Mode: &ipc.Mode{Chinese: false, FullPunct: true}})

	frames := p.roundtrip(ipc.TypeFocusIn, "s-2", ipc.FocusIn{App: "b.exe"})
	if u := decodeUpdate(t, frames[0]); u.Mode.Chinese {
		t.Fatalf("新会话应继承英文模式: %+v", u.Mode)
	}
	q := dial(t, 2, mgr)
	q.send(ipc.TypeHello, "", ipc.Hello{V: 1, Build: "test"})
	var w ipc.Welcome
	if err := json.Unmarshal(q.recv().Payload, &w); err != nil || w.Cfg.Chinese {
		t.Fatalf("welcome 应下发当前模式: %+v", w)
	}
}
