package main

import (
	"bytes"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"testing"

	"github.com/luobochuanqi/silk-ime/internal/dict"
	"github.com/luobochuanqi/silk-ime/internal/engine"
	"github.com/luobochuanqi/silk-ime/internal/ipc"
	"github.com/luobochuanqi/silk-ime/internal/session"
)

// 端到端协议测试：以 net.Pipe 模拟 shim，走真实词库/引擎/会话栈，
// 断言握手、候选产生与上屏的线行为（不依赖 Windows 与渲染）。

type testPipe struct {
	t    *testing.T
	conn net.Conn
}

func newTestPipe(t *testing.T) *testPipe {
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
	mgr := session.NewManager(eng, "")

	client, server := net.Pipe()
	t.Cleanup(func() { client.Close(); server.Close() })
	c := &conn{nc: server, mgr: mgr, snapshot: cfgSnapshot{PageSize: 5, Chinese: true, FullPunct: true}}
	go c.run()
	return &testPipe{t: t, conn: client}
}

func (p *testPipe) send(typ, sid string, payload any) {
	p.t.Helper()
	raw, err := ipc.Encode(ipc.Envelope{V: 1, Type: typ, SID: sid, Payload: marshalPayload(p.t, payload)})
	if err != nil {
		p.t.Fatalf("编码 %s: %v", typ, err)
	}
	if err := ipc.WriteFrame(p.conn, raw); err != nil {
		p.t.Fatalf("发送 %s: %v", typ, err)
	}
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

func TestHandshakeAndTyping(t *testing.T) {
	p := newTestPipe(t)
	p.send("hello", "", helloMsg{V: 1, Caps: 0, Build: "test"})
	if env := p.recv(); env.Type != "welcome" {
		t.Fatalf("期望 welcome，得到 %s", env.Type)
	}

	p.send("focus_in", "s-1", focusInMsg{App: "notepad.exe", HWND: 42, LangHint: "zh-CN"})
	if env := p.recv(); env.Type != "update" {
		t.Fatalf("focus_in 后期望 update，得到 %s", env.Type)
	}

	// 逐键输入 nihao（键码：N=14 I=9 H=8 A=1 O=15）
	for _, code := range []uint16{14, 9, 8, 1, 15} {
		p.send("key", "s-1", keyMsg{Code: code, Down: true, TS: 100})
		p.recv() // update
	}
	p.send("key", "s-1", keyMsg{Code: 37, Down: true, TS: 200}) // 空格上屏首选
	env := p.recv()
	if env.Type != "update" {
		t.Fatalf("空格后期望 update，得到 %s", env.Type)
	}
	env = p.recv()
	if env.Type != "commit" {
		t.Fatalf("空格后期望 commit，得到 %s", env.Type)
	}
	if got := string(env.Payload); !containsWord(t, env.Payload, "你好") {
		t.Fatalf("commit 应含 你好: %s", got)
	}
}

func TestUnhandshakedRejected(t *testing.T) {
	p := newTestPipe(t)
	p.send("key", "s-1", keyMsg{Code: 14, Down: true, TS: 1})
	env := p.recv()
	if env.Type != "error" {
		t.Fatalf("未握手发消息应报错，得到 %s", env.Type)
	}
}

func TestUnknownSession(t *testing.T) {
	p := newTestPipe(t)
	p.send("hello", "", helloMsg{V: 1, Caps: 0, Build: "test"})
	p.recv()
	p.send("key", "s-x", keyMsg{Code: 14, Down: true, TS: 1})
	if env := p.recv(); env.Type != "error" {
		t.Fatalf("未知会话应报错，得到 %s", env.Type)
	}
}

func marshalPayload(t *testing.T, v any) []byte {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("载荷编码: %v", err)
	}
	return b
}

func containsWord(t *testing.T, raw []byte, word string) bool {
	t.Helper()
	return bytes.Contains(raw, []byte(word))
}
